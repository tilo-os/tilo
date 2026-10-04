//! GStreamer decode backend. It:
//!
//! * ranks hardware decoders above software ones ([`crate::hw`]) and lets
//!   `playbin3` autoplug, then records the selected element;
//! * negotiates the best memory the consumer declared it can take —
//!   `memory:DMABuf` first, then planar system memory, and only RGBA when the
//!   consumer says that is all it can paint;
//! * seeks with `ACCURATE` and reads the **preroll** sample while the pipeline
//!   stays in `PAUSED`, so scrubbing lands on the requested frame and never
//!   starts the audio sink;
//! * runs clock-driven playback when asked to play.

use std::collections::VecDeque;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use gstreamer as gst;
use gstreamer::glib::translate::{FromGlib, IntoGlib};
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;

use crate::event::{MediaEvent, VideoFrame};
use crate::frame::{Frame, FrameAcceptance, FramePayload};
use crate::gst_frames::frame_from_sample;
use crate::hw::{DecoderEntry, DecoderKind, HwPolicy, HwReport, rank_for};
use crate::source::Source;
use crate::{MediaChapter, MediaTrack};

/// Raise every decoder in the registry to the rank [`HwPolicy`] assigns it.
///
/// Runs once per process, right after `gst::init`. Without this, a build whose
/// `va*` elements ship at rank `NONE` never autoplugs them however capable the
/// GPU is.
fn apply_decoder_ranks(policy: HwPolicy) {
    let registry = gst::Registry::get();
    for feature in registry.features(gst::ElementFactory::static_type()) {
        let name = feature.name().to_string();
        if !crate::hw::looks_like_video_decoder(&name) {
            continue;
        }
        let kind = DecoderKind::classify(&name);
        // Safety: `Rank` is a transparent wrapper over the C `gint` rank, and
        // any value is meaningful — GStreamer's own constants are just points
        // on that scale.
        // Ranks are a small scale (`GST_RANK_PRIMARY` is 256), so the
        // conversion is exact; a rank that somehow did not fit would be
        // "rank this as high as possible", which is what saturating gives.
        let rank = i32::try_from(rank_for(policy, kind)).unwrap_or(i32::MAX);
        let want = unsafe { gst::Rank::from_glib(rank) };
        if feature.rank() != want {
            feature.set_rank(want);
        }
    }
    tracing::debug!("media: decoder ranks applied for policy {policy:?}");
}

/// Every video decoder in the `GStreamer` registry, with the rank it currently
/// carries.
///
/// This is what makes "no hardware decode happened" a *checkable* statement
/// rather than an excuse: a caller (or a test) can ask whether a hardware
/// decoder for the stream even exists on this machine before concluding
/// anything about why a software one was chosen.
///
/// Initialises `GStreamer` if it is not already up; returns an empty list if it
/// cannot be initialised at all.
pub fn available_video_decoders() -> Vec<DecoderEntry> {
    if gst::init().is_err() {
        return Vec::new();
    }
    crate::extensions::refresh_registry();
    let registry = gst::Registry::get();
    let mut out: Vec<DecoderEntry> = registry
        .features(gst::ElementFactory::static_type())
        .into_iter()
        .filter_map(|f| {
            let name = f.name().to_string();
            if !crate::hw::looks_like_video_decoder(&name) {
                return None;
            }
            Some(DecoderEntry {
                kind: DecoderKind::classify(&name),
                // `GstRank` is a `gint`. A negative rank means "below
                // `GST_RANK_NONE`", which this scale expresses as zero.
                rank: u32::try_from(f.rank().into_glib()).unwrap_or(0),
                name,
            })
        })
        .collect();
    out.sort_by(|a, b| b.rank.cmp(&a.rank).then_with(|| a.name.cmp(&b.name)));
    out
}

/// The video decoders in the registry that can handle `media_type` (a caps
/// media type such as `video/x-h264`), hardware ones first.
///
/// Used to answer "does this machine have a hardware decoder for *this*
/// stream?" — the question that separates "hardware decode is broken" from
/// "this box has no hardware decoder for this codec".
pub fn decoders_for(media_type: &str) -> Vec<DecoderEntry> {
    if gst::init().is_err() {
        return Vec::new();
    }
    let Ok(caps) = gst::Caps::from_str(media_type) else {
        return Vec::new();
    };
    available_video_decoders()
        .into_iter()
        .filter(|d| gst::ElementFactory::find(&d.name).is_some_and(|f| f.can_sink_any_caps(&caps)))
        .collect()
}

/// The caps an appsink advertises for a given [`FrameAcceptance`].
///
/// Structures are ordered best-first: `GStreamer` negotiates the first one both
/// sides can satisfy, so a decoder that can export dmabufs will, and one that
/// cannot falls through to planar system memory without a colour conversion.
fn sink_caps(acceptance: FrameAcceptance) -> gst::Caps {
    #[cfg(unix)]
    const NATIVE_CAPS: &str = "video/x-raw(memory:DMABuf); video/x-raw, format = (string) { NV12, P010_10LE, RGBA, BGRA, BGRx, RGBx }";
    // Windows decoders can supply real planar or RGB system-memory frames.
    // DMA-BUF is a Unix fd protocol, not a Windows shared-texture handle.
    #[cfg(not(unix))]
    const NATIVE_CAPS: &str =
        "video/x-raw, format = (string) { NV12, P010_10LE, RGBA, BGRA, BGRx, RGBx }";
    match acceptance {
        // The compatibility surface: system-memory RGBA, exactly what
        // `VideoFrame` has always carried.
        FrameAcceptance::Rgba => gst::Caps::builder("video/x-raw")
            .field("format", "RGBA")
            .build(),
        FrameAcceptance::Native => gst::Caps::from_str(NATIVE_CAPS).unwrap_or_else(|_| {
            // Unreachable: the argument is a string literal. Falling back to
            // the RGBA surface rather than panicking means a parse failure
            // costs the zero-copy path, not the process.
            gst::Caps::builder("video/x-raw")
                .field("format", "RGBA")
                .build()
        }),
    }
}

/// The video sink bin. Under [`FrameAcceptance::Rgba`] a `videoconvert` stands
/// in front of the sink so any decoder output reaches RGBA; under
/// [`FrameAcceptance::Native`] the sink is bare and negotiation decides.
fn build_video_sink(acceptance: FrameAcceptance) -> Option<(gst::Element, gst_app::AppSink)> {
    let appsink = gst_app::AppSink::builder()
        .name("tilo_vsink")
        .caps(&sink_caps(acceptance))
        // `sync=true` paces frames to their PTS during playback. Scrubbing
        // reads the preroll instead, which is not paced at all.
        .sync(true)
        .max_buffers(8)
        // Start paused, so start without dropping — see `set_drop_policy`.
        .drop(false)
        .enable_last_sample(false)
        .build();

    // A dmabuf producer refuses to negotiate `memory:DMABuf` unless the sink
    // advertises `GstVideoMeta` support — without it the plane offsets and
    // strides would have nowhere to travel. Declining to answer this query is
    // exactly why a naive `appsink` gets "DMABuf caps negotiated without the
    // mandatory support of VideoMeta" and falls back to system memory.
    appsink.set_callbacks(
        gst_app::AppSinkCallbacks::builder()
            .propose_allocation(|_sink, query| {
                query.add_allocation_meta::<gst_video::VideoMeta>(None);
                true
            })
            .build(),
    );

    match acceptance {
        FrameAcceptance::Native => {
            let element = appsink.clone().upcast::<gst::Element>();
            Some((element, appsink))
        }
        FrameAcceptance::Rgba => {
            let bin = gst::Bin::builder().name("tilo_vbin").build();
            let convert = gst::ElementFactory::make("videoconvert").build().ok()?;
            let sink_el = appsink.clone().upcast::<gst::Element>();
            bin.add_many([&convert, &sink_el]).ok()?;
            convert.link(&sink_el).ok()?;
            let pad = convert.static_pad("sink")?;
            let ghost = gst::GhostPad::with_target(&pad).ok()?;
            bin.add_pad(&ghost).ok()?;
            Some((bin.upcast::<gst::Element>(), appsink))
        }
    }
}

/// The seek precision a scrub or jump asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SeekMode {
    /// Decode from the preceding keyframe up to the requested time and present
    /// **that** frame. What an editor needs; costs a short decode run.
    Accurate,
    /// Snap to the nearest keyframe. Cheap, and up to a GOP away from the
    /// requested time — useful only for coarse, fast-moving scrubbing.
    Keyframe,
}

impl SeekMode {
    fn flags(self) -> gst::SeekFlags {
        match self {
            SeekMode::Accurate => gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE,
            SeekMode::Keyframe => {
                gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT | gst::SeekFlags::SNAP_NEAREST
            }
        }
    }
}

/// Shared state the bus watch and the `deep-element-added` handler write into.
#[derive(Default)]
struct Observed {
    report: HwReport,
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "four unrelated facts about a live pipeline — transport state, \
              whether one event has been emitted, whether a flushing seek is \
              outstanding, and whether audio is muted. They are independent, \
              so an enum over them would enumerate combinations that all \
              legitimately occur"
)]
pub struct GstState {
    playbin: gst::Element,
    appsink: gst_app::AppSink,
    bus: gst::Bus,
    acceptance: FrameAcceptance,
    observed: Arc<Mutex<Observed>>,

    latest: Option<Frame>,
    /// [`VideoFrame`] view of `latest`, materialized on demand and cached so
    /// repeated `latest_frame()` calls while paused cost nothing.
    latest_rgba: Option<VideoFrame>,

    playing: bool,
    duration_announced: bool,
    last_position: Option<u64>,
    last_buffering: Option<u8>,
    buffering: u8,
    pending: VecDeque<MediaEvent>,

    /// Set while a flushing seek is in flight; cleared on `AsyncDone`. Used to
    /// know whether a preroll sample is the one that was asked for.
    seek_in_flight: bool,
    /// The position the last seek asked for, so `position_ms` reports the
    /// requested time rather than a stale query result while the seek settles.
    seek_target_ms: Option<u64>,
    /// A seek asked for before the first preroll completed, applied once the
    /// pipeline reaches PAUSED. A later request replaces it.
    deferred_seek: Option<(u64, SeekMode)>,

    volume: f32,
    muted: bool,
    rate: f32,
    policy: HwPolicy,

    /// Monotonic count of pictures stored since this pipeline was built.
    /// Paired with `frames_at_seek`, this is how a caller distinguishes "the
    /// frame for the position I just asked for" from "the frame that was
    /// already on screen".
    frames_stored: u64,
    /// `frames_stored` at the moment the last seek was accepted.
    frames_at_seek: u64,
    /// Flattened, trusted container TOC. The bus publishes it asynchronously
    /// after discovery, so callers can poll this without blocking decode.
    chapters: Vec<MediaChapter>,
}

impl GstState {
    /// Build the pipeline with no source yet. `None` when `GStreamer` cannot
    /// initialise or the sink chain cannot be built, so the engine falls back
    /// to the synthesized backend.
    pub fn new(acceptance: FrameAcceptance, policy: HwPolicy, audio_output: bool) -> Option<Self> {
        gst::init().ok()?;
        // Pick up plugins a codec-pack merge added (or an unmerge removed)
        // before ranking, so new hardware decoders get their rank too.
        crate::extensions::refresh_registry();
        // Ranks are re-applied per pipeline rather than once per process.
        // GStreamer's registry ranks are global, so the most recently built
        // engine's policy is the one in force — but re-applying makes the
        // policy a property of the engine's configuration rather than of
        // whichever code happened to touch the registry first, which is what
        // lets a caller (and a test) actually choose it.
        apply_decoder_ranks(policy);

        // Prefer `playbin`: unlike `playbin3` in current GStreamer releases,
        // it exposes the stable `n-audio` / `current-audio` and text-stream
        // properties required for a user to inspect and switch embedded
        // tracks. `playbin3` remains a decode fallback on stripped systems.
        let playbin = gst::ElementFactory::make("playbin")
            .build()
            .or_else(|_| gst::ElementFactory::make("playbin3").build())
            .ok()?;

        let (video_sink, appsink) = build_video_sink(acceptance)?;
        playbin.set_property("video-sink", &video_sink);

        // Audio through a real sink so playback is audible, with an explicit
        // `volume` element: `playbin`'s own volume property re-negotiates the
        // stream, while the element is a plain gain stage that can be moved
        // per-source without disturbing the pipeline.
        // An engine that never plays sound decodes no audio stream and opens
        // no audio device.
        if !audio_output {
            playbin.set_property_from_str("flags", "video+text+deinterlace+soft-colorbalance");
        } else if let Ok(audio_bin) = gst::parse::bin_from_description(
            "audioconvert ! audioresample ! volume name=tilo_gain ! autoaudiosink name=tilo_asink",
            true,
        ) {
            playbin.set_property("audio-sink", &audio_bin);
        }
        playbin.set_property("mute", false);
        playbin.set_property("volume", 0.8f64);

        let observed = Arc::new(Mutex::new(Observed::default()));

        // Watch every element the pipeline instantiates, however deeply
        // nested, and record the decoder. This is the evidence that hardware
        // decode engaged: the name of the element GStreamer chose.
        {
            let observed = observed.clone();
            playbin.connect("deep-element-added", false, move |vals| {
                if let Some(element) = vals.get(2).and_then(|v| v.get::<gst::Element>().ok()) {
                    let name = element.name().to_string();
                    if let Ok(mut o) = observed.lock()
                        && o.report.note_element(&name)
                    {
                        let kind = o.report.decoder_kind.unwrap_or(DecoderKind::Software);
                        tracing::info!(
                            "media: decoder {name} ({}, {})",
                            kind.as_str(),
                            if kind.is_hardware() {
                                "hardware"
                            } else {
                                "software"
                            }
                        );
                    }
                }
                None
            });
        }

        let bus = playbin.bus()?;
        Some(Self {
            playbin,
            appsink,
            bus,
            acceptance,
            observed,
            latest: None,
            latest_rgba: None,
            playing: false,
            duration_announced: false,
            last_position: None,
            last_buffering: None,
            buffering: 100,
            pending: VecDeque::new(),
            seek_in_flight: false,
            seek_target_ms: None,
            deferred_seek: None,
            volume: 0.8,
            muted: false,
            rate: 1.0,
            policy,
            frames_stored: 0,
            frames_at_seek: 0,
            chapters: Vec::new(),
        })
    }

    pub fn acceptance(&self) -> FrameAcceptance {
        self.acceptance
    }

    /// The hardware-decode policy this pipeline was built under.
    pub fn policy(&self) -> HwPolicy {
        self.policy
    }

    /// What the pipeline actually plugged. Empty until the first frame has
    /// prerolled — never a claim made ahead of the evidence.
    pub fn hw_report(&self) -> HwReport {
        self.observed
            .lock()
            .map(|o| o.report.clone())
            .unwrap_or_default()
    }

    pub fn open(&mut self, source: &Source, volume: f32) {
        let _ = self.playbin.set_state(gst::State::Null);
        self.latest = None;
        self.latest_rgba = None;
        self.playing = false;
        self.duration_announced = false;
        self.last_position = None;
        self.last_buffering = None;
        self.buffering = 100;
        self.pending.clear();
        self.seek_in_flight = false;
        self.seek_target_ms = None;
        self.deferred_seek = None;
        self.frames_stored = 0;
        self.frames_at_seek = 0;
        self.chapters.clear();
        if let Ok(mut o) = self.observed.lock() {
            o.report = HwReport::default();
        }

        // Another engine may have been built since, leaving the registry on
        // its policy, and extensions may have changed since this engine was
        // built. Re-assert ours before the decoder is autoplugged.
        crate::extensions::refresh_registry();
        apply_decoder_ranks(self.policy);
        self.set_drop_policy(false);
        let uri = match source.to_uri() {
            Ok(uri) => uri,
            Err(error) => {
                self.pending.push_back(MediaEvent::Error(error.to_string()));
                return;
            }
        };
        self.playbin.set_property("uri", uri);
        self.volume = volume.clamp(0.0, 1.0);
        self.apply_gain();
        // Preroll: a first frame and the duration become available without
        // ever entering PLAYING, so opening a clip is silent.
        let _ = self.playbin.set_state(gst::State::Paused);
    }

    /// Block until the pipeline finishes its current asynchronous state change
    /// or `timeout` elapses. Returns whether it settled.
    ///
    /// Used by the frame-exact path: after a flushing seek the new preroll is
    /// only guaranteed to be present once the state change completes.
    pub fn wait_settled(&mut self, timeout: std::time::Duration) -> bool {
        // `as_millis` is `u128`; a timeout that does not fit `u64` is
        // "wait effectively forever", which is what saturating gives.
        let wait_ms = u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX);
        let settled = matches!(
            self.playbin.state(gst::ClockTime::from_mseconds(wait_ms)).0,
            Ok(gst::StateChangeSuccess::Success | gst::StateChangeSuccess::NoPreroll)
        );
        if settled {
            self.seek_in_flight = false;
            self.apply_deferred_seek();
        }
        settled
    }

    /// Choose whether the sink discards buffers when its queue fills.
    ///
    /// The two modes want opposite behaviour, and getting it wrong is a
    /// **silent two-frame error**, not a stall:
    ///
    /// * **Scrubbing / paused.** After a flushing seek the decoder pushes the
    ///   target frame and then races ahead. With `drop=true` the sink discards
    ///   the *oldest* buffer to make room for newer ones, so the frame that
    ///   was seeked to is thrown away and a later one is served in its place —
    ///   a constant offset that looks exactly like an inaccurate seek. With
    ///   `drop=false` the sink blocks the decoder instead and the frame at the
    ///   requested time survives.
    /// * **Playing.** Dropping is right: a slow consumer must not make the
    ///   picture lag the clock.
    fn set_drop_policy(&self, playing: bool) {
        self.appsink.set_property("drop", playing);
    }

    pub fn set_playing(&mut self, playing: bool) {
        self.playing = playing;
        self.set_drop_policy(playing);
        let state = if playing {
            gst::State::Playing
        } else {
            gst::State::Paused
        };
        let _ = self.playbin.set_state(state);
        if playing {
            // Once running, the pipeline clock owns the playhead; a stale seek
            // target would pin the reported position.
            self.seek_target_ms = None;
        }
    }

    /// Change speed without reconstructing the decode graph. `GStreamer` models
    /// this as a seek at the current position; the FLUSH avoids presenting
    /// queued samples at the old clock rate.
    pub fn set_rate(&mut self, rate: f32) {
        self.rate = rate.clamp(0.25, 4.0);
        let position = gst::ClockTime::from_mseconds(self.position_ms());
        let flags = gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE;
        if let Err(error) = self.playbin.seek(
            f64::from(self.rate),
            flags,
            gst::SeekType::Set,
            position,
            gst::SeekType::None,
            gst::ClockTime::NONE,
        ) {
            tracing::warn!(%error, rate = self.rate, "media: playback-rate change refused");
        } else {
            self.seek_in_flight = true;
            self.seek_target_ms = Some(position.mseconds());
            self.frames_at_seek = self.frames_stored;
        }
    }

    pub fn audio_tracks(&self) -> Vec<MediaTrack> {
        self.tracks(
            "n-audio",
            "current-audio",
            "get-audio-pad",
            "get-audio-tags",
            "Audio track",
        )
    }

    pub fn subtitle_tracks(&self) -> Vec<MediaTrack> {
        self.tracks(
            "n-text",
            "current-text",
            "get-text-pad",
            "get-text-tags",
            "Subtitle track",
        )
    }

    pub fn chapters(&self) -> Vec<MediaChapter> {
        self.chapters.clone()
    }

    fn tracks(
        &self,
        count_property: &str,
        current_property: &str,
        pad_signal: &str,
        tag_signal: &str,
        prefix: &str,
    ) -> Vec<MediaTrack> {
        // `playbin3` and older `playbin` installations do not all expose the
        // same stream-selection properties. A missing optional capability is
        // an empty truthful enumeration, never a GLib property panic.
        if self.playbin.find_property(count_property).is_none()
            || self.playbin.find_property(current_property).is_none()
        {
            return Vec::new();
        }
        let count = self.playbin.property::<i32>(count_property).max(0);
        let current = self.playbin.property::<i32>(current_property);
        (0..count)
            .filter_map(|index| {
                let index = u32::try_from(index).ok()?;
                // The stream-id is emitted by the demuxer and is the only
                // safe identity to publish to agents. Do not invent an ID
                // from a transient array position: it could target another
                // stream after renegotiation.
                let pad = self
                    .playbin
                    .emit_by_name::<Option<gst::Pad>>(pad_signal, &[&i32::try_from(index).ok()?])?;
                let id = pad.stream_id()?.to_string();
                Some(MediaTrack {
                    id,
                    label: self
                        .track_label(tag_signal, index)
                        .unwrap_or_else(|| format!("{prefix} {}", index.saturating_add(1))),
                    index,
                    selected: i32::try_from(index).ok() == Some(current),
                })
            })
            .collect()
    }

    fn track_label(&self, tag_signal: &str, index: u32) -> Option<String> {
        let index = i32::try_from(index).ok()?;
        let tags = self
            .playbin
            .emit_by_name::<Option<gst::TagList>>(tag_signal, &[&index])?;
        tags.index::<gst::tags::Title>(0)
            .map(|value| value.get().to_string())
            .or_else(|| {
                tags.index::<gst::tags::LanguageName>(0)
                    .map(|value| value.get().to_string())
            })
            .filter(|label| !label.trim().is_empty())
    }

    pub fn select_audio_track(&self, index: u32) -> bool {
        self.select_track("n-audio", "current-audio", Some(index))
    }

    pub fn select_subtitle_track(&self, index: Option<u32>) -> bool {
        self.select_track("n-text", "current-text", index)
    }

    fn select_track(
        &self,
        count_property: &str,
        current_property: &str,
        index: Option<u32>,
    ) -> bool {
        if self.playbin.find_property(count_property).is_none()
            || self.playbin.find_property(current_property).is_none()
        {
            return false;
        }
        let count = self.playbin.property::<i32>(count_property).max(0);
        let requested = index.and_then(|i| i32::try_from(i).ok()).unwrap_or(-1);
        if requested >= count || requested < -1 {
            return false;
        }
        self.playbin.set_property(current_property, requested);
        true
    }

    pub fn is_playing(&self) -> bool {
        self.playing
    }

    /// Seek to `ms`.
    ///
    /// `SeekMode::Accurate` decodes forward from the preceding keyframe so the
    /// frame presented is the one at `ms`, not the keyframe before it. The
    /// pipeline stays in whatever state it is in — a paused pipeline
    /// re-prerolls and the new frame arrives on the preroll path, so scrubbing
    /// never starts the audio sink.
    pub fn seek(&mut self, ms: u64, mode: SeekMode) -> bool {
        // Before the first preroll completes, decodebin may still be
        // autoplugging the decoder, and a flushing seek GStreamer accepts in
        // that window can leave the decoder's caps unnegotiated, so no
        // picture ever arrives. Hold it until the pipeline reaches PAUSED.
        let (_, current, _) = self.playbin.state(gst::ClockTime::ZERO);
        if current < gst::State::Paused {
            self.deferred_seek = Some((ms, mode));
            self.mark_seek(ms);
            return true;
        }
        self.deferred_seek = None;
        self.issue_seek(ms, mode)
    }

    /// Issue a seek held back by [`Self::seek`], once the first preroll is in.
    fn apply_deferred_seek(&mut self) {
        let Some((ms, mode)) = self.deferred_seek else {
            return;
        };
        let (_, current, _) = self.playbin.state(gst::ClockTime::ZERO);
        if current >= gst::State::Paused && self.issue_seek(ms, mode) {
            self.deferred_seek = None;
        }
    }

    fn issue_seek(&mut self, ms: u64, mode: SeekMode) -> bool {
        let pos = gst::ClockTime::from_mseconds(ms);
        match self.playbin.seek_simple(mode.flags(), pos) {
            Ok(()) => {
                self.mark_seek(ms);
                true
            }
            Err(e) => {
                tracing::debug!("media: seek to {ms}ms not accepted: {e}");
                false
            }
        }
    }

    fn mark_seek(&mut self, ms: u64) {
        self.seek_in_flight = true;
        self.seek_target_ms = Some(ms);
        // Anything already held belongs to the *previous* position. Record
        // the watermark rather than clearing the frame: the preview keeps
        // painting the old picture (no flicker) while `frame_is_post_seek`
        // reports, truthfully, that it is stale.
        self.frames_at_seek = self.frames_stored;
    }

    /// Volume in `0.0..=1.0`, applied to the gain element so the change does
    /// not disturb the negotiated stream.
    pub fn set_volume(&mut self, v: f32) {
        self.volume = v.clamp(0.0, 1.0);
        self.apply_gain();
    }

    pub fn volume(&self) -> f32 {
        self.volume
    }

    /// Silence the output without losing the volume setting.
    pub fn set_muted(&mut self, muted: bool) {
        self.muted = muted;
        self.apply_gain();
    }

    pub fn is_muted(&self) -> bool {
        self.muted
    }

    /// Push the current volume/mute to the pipeline. Prefers the `volume`
    /// element in the audio bin; falls back to playbin's own property when the
    /// bin could not be built.
    fn apply_gain(&self) {
        let level = if self.muted {
            0.0
        } else {
            f64::from(self.volume)
        };
        if let Some(gain) = self.gain_element() {
            gain.set_property("volume", level);
        } else {
            self.playbin.set_property("volume", level);
        }
        self.playbin.set_property("mute", self.muted);
    }

    /// The `volume` element in the audio sink bin, when one was built.
    fn gain_element(&self) -> Option<gst::Element> {
        let sink = self
            .playbin
            .property::<Option<gst::Element>>("audio-sink")?;
        sink.downcast::<gst::Bin>().ok()?.by_name("tilo_gain")
    }

    /// The audio sink element, the entry point for routing or monitoring the
    /// output (its `device` property, a `PipeWire` target node, a tee for a
    /// meter). `None` when the audio bin could not be built.
    pub fn audio_sink(&self) -> Option<gst::Element> {
        let sink = self
            .playbin
            .property::<Option<gst::Element>>("audio-sink")?;
        sink.downcast::<gst::Bin>().ok()?.by_name("tilo_asink")
    }

    pub fn position_ms(&self) -> u64 {
        // While a seek is settling the pipeline still reports the old
        // position; the requested target is the honest answer until the new
        // frame lands.
        if let (Some(target), true) = (self.seek_target_ms, self.seek_in_flight) {
            return target;
        }
        self.playbin
            .query_position::<gst::ClockTime>()
            .map(gst::ClockTime::mseconds)
            .or(self.seek_target_ms)
            .or(self.last_position)
            .or_else(|| self.latest.as_ref().map(|f| f.pts_ms))
            .unwrap_or(0)
    }

    pub fn duration_ms(&self) -> Option<u64> {
        self.playbin
            .query_duration::<gst::ClockTime>()
            .map(gst::ClockTime::mseconds)
    }

    pub fn buffering_percent(&self) -> u8 {
        self.buffering
    }

    /// The most recent picture in its native memory.
    ///
    /// May belong to a previous position if a seek is still in flight — see
    /// [`Self::frame_is_post_seek`].
    pub fn latest(&self) -> Option<&Frame> {
        self.latest.as_ref()
    }

    /// Whether the held picture was decoded *after* the most recent seek.
    ///
    /// Without this, a caller that waits for "a frame to exist" is satisfied
    /// instantly by the frame that was already on screen, and every seek after
    /// the first appears to return the previous position's picture. The frame
    /// is deliberately not cleared on seek — the preview keeps painting the
    /// old one so scrubbing does not flicker — so staleness has to be
    /// reported rather than inferred from absence.
    pub fn frame_is_post_seek(&self) -> bool {
        // The first preroll is not the picture a deferred seek asked for.
        self.deferred_seek.is_none() && self.frames_stored > self.frames_at_seek
    }

    /// The most recent picture as system-memory RGBA.
    ///
    /// Under [`FrameAcceptance::Rgba`] — the default, and what every existing
    /// consumer of [`VideoFrame`] uses — the pipeline negotiates RGBA, so this
    /// is always populated whenever a frame exists.
    ///
    /// Under [`FrameAcceptance::Native`] a picture may be a dmabuf or planar
    /// YUV, and this returns `None` rather than performing a hidden GPU→CPU
    /// readback or a per-call colour conversion. Those consumers read
    /// [`Self::latest`] instead.
    pub fn latest_frame(&self) -> Option<&VideoFrame> {
        self.latest_rgba.as_ref()
    }

    /// Take whatever picture the sink is holding.
    ///
    /// **Order matters, and getting it wrong loses frames silently.** `appsink`
    /// serves both pull APIs from one internal object queue: asking for a
    /// regular sample pops from that queue and *discards* anything that is not
    /// one — including a preroll. So calling `try_pull_sample` first, then
    /// falling back to `try_pull_preroll`, throws the preroll away and then
    /// finds nothing, with no error anywhere. While paused (scrubbing, or
    /// freshly opened) the preroll is the only picture that exists, so it has
    /// to be asked for first.
    fn drain_samples(&mut self) -> bool {
        let mut got = false;

        // Paused, or playing but not yet seeded: the preroll is the picture.
        if (!self.playing || self.latest.is_none())
            && let Some(sample) = self.appsink.try_pull_preroll(gst::ClockTime::ZERO)
            && let Some(frame) = frame_from_sample(&sample, self.acceptance)
        {
            self.record_caps(&sample);
            self.store(frame);
            got = true;
        }

        // Playing: drain to the newest so the surface never lags the pipeline.
        if self.playing {
            while let Some(sample) = self.appsink.try_pull_sample(gst::ClockTime::ZERO) {
                if let Some(frame) = frame_from_sample(&sample, self.acceptance) {
                    self.record_caps(&sample);
                    self.store(frame);
                    got = true;
                }
            }
        }
        got
    }

    /// Keep `frame` as the current picture, refreshing the RGBA view that
    /// [`Self::latest_frame`] hands out.
    ///
    /// The view is built here rather than lazily so `latest_frame` can stay a
    /// `&self` accessor. It costs nothing on the RGBA path (the payload is
    /// already RGBA and only its `Arc` is cloned) and is deliberately *not*
    /// built for a planar or dmabuf payload — a `Native` consumer reads
    /// [`Self::latest`], and converting behind its back would reintroduce the
    /// per-frame CPU cost this path removes.
    fn store(&mut self, frame: Frame) {
        // Saturating: this counter only ever answers "is the picture newer
        // than the last seek", and a wrap would make a fresh frame look
        // stale forever. 2^64 frames is 24 billion years at 24fps.
        self.frames_stored = self.frames_stored.saturating_add(1);
        self.latest_rgba = match frame.payload {
            FramePayload::Rgba(_) => frame.to_video_frame(),
            _ => None,
        };
        self.latest = Some(frame);
    }

    /// Record the caps the sink negotiated, which describe the frame memory.
    fn record_caps(&self, sample: &gst::Sample) {
        if let Some(caps) = sample.caps()
            && let Ok(mut o) = self.observed.lock()
            && o.report.negotiated_caps.is_none()
        {
            o.report.negotiated_caps = Some(caps.to_string());
        }
    }

    pub fn poll(&mut self) -> Vec<MediaEvent> {
        let mut events: Vec<MediaEvent> = self.pending.drain(..).collect();

        if !self.duration_announced
            && let Some(ms) = self.duration_ms()
        {
            events.push(MediaEvent::DurationKnown { ms });
            self.duration_announced = true;
        }

        while let Some(msg) = self.bus.pop() {
            use gst::MessageView;
            match msg.view() {
                MessageView::Buffering(b) => {
                    // Clamped to 0..=100 on the same line, so the
                    // conversion cannot fail; 0 would be the honest reading
                    // of a percentage that somehow was not one.
                    let pct = u8::try_from(b.percent().clamp(0, 100)).unwrap_or(0);
                    self.buffering = pct;
                    if self.last_buffering != Some(pct) {
                        self.last_buffering = Some(pct);
                        events.push(MediaEvent::Buffering { percent: pct });
                    }
                }
                MessageView::AsyncDone(_) => {
                    // The flushing seek finished; the sink now holds the frame
                    // for the requested time.
                    self.seek_in_flight = self.deferred_seek.is_some();
                }
                MessageView::Eos(_) => {
                    self.playing = false;
                    events.push(MediaEvent::Ended);
                }
                MessageView::Error(e) => {
                    self.playing = false;
                    events.push(MediaEvent::Error(e.error().to_string()));
                }
                MessageView::Toc(toc) => {
                    let (toc, _) = toc.toc();
                    self.chapters = chapters_from_toc(&toc);
                }
                _ => {}
            }
        }

        self.drain_samples();
        self.apply_deferred_seek();

        let pos = self.position_ms();
        if self.last_position != Some(pos) {
            self.last_position = Some(pos);
            events.push(MediaEvent::Position { ms: pos });
        }

        events
    }
}

/// Flatten a container TOC without inventing any labels. Nested editions and
/// titles are traversed because containers commonly place chapter entries
/// beneath an edition node; only actual `Chapter` entries become seek targets.
fn chapters_from_toc(toc: &gst::Toc) -> Vec<MediaChapter> {
    fn visit(entry: &gst::TocEntry, out: &mut Vec<MediaChapter>) {
        if entry.entry_type() == gst::TocEntryType::Chapter
            && let Some((start_ns, _)) = entry.start_stop_times()
            && let Ok(start_ns) = u64::try_from(start_ns)
        {
            let label = entry
                .tags()
                .and_then(|tags| {
                    tags.index::<gst::tags::Title>(0)
                        .map(|value| value.get().to_string())
                })
                .filter(|label| !label.trim().is_empty())
                .unwrap_or_else(|| entry.uid().to_string());
            out.push(MediaChapter {
                id: entry.uid().to_string(),
                label,
                start_ms: start_ns / 1_000_000,
            });
        }
        for child in entry.sub_entries() {
            visit(&child, out);
        }
    }

    let mut chapters = Vec::new();
    for entry in toc.entries() {
        visit(&entry, &mut chapters);
    }
    chapters.sort_by(|a, b| a.start_ms.cmp(&b.start_ms).then_with(|| a.id.cmp(&b.id)));
    chapters.dedup_by(|a, b| a.id == b.id);
    chapters
}

impl Drop for GstState {
    fn drop(&mut self) {
        let _ = self.playbin.set_state(gst::State::Null);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Caps construction needs a live registry. Initialising is idempotent and
    /// safe to call from every test.
    fn init() {
        gst::init().expect("GStreamer initialises in the test environment");
    }

    #[test]
    fn rgba_caps_and_seek_flags_are_requested_exactly() {
        // Phase `rgba_acceptance_asks_only_for_system_memory_rgba`.
        {
            init();
            // The compatibility path must never negotiate a GPU buffer that its
            // consumer cannot paint.
            let caps = sink_caps(FrameAcceptance::Rgba);
            let s = caps.to_string();
            assert!(s.contains("RGBA"), "{s}");
            assert!(
                !s.contains("DMABuf"),
                "RGBA-only consumers must not be offered dmabuf: {s}"
            );
        }
        // Phase `accurate_seek_carries_the_accurate_flag_and_keyframe_seek_does_not`.
        {
            let a = SeekMode::Accurate.flags();
            assert!(
                a.contains(gst::SeekFlags::ACCURATE),
                "exact frame requires ACCURATE"
            );
            assert!(a.contains(gst::SeekFlags::FLUSH));
            assert!(
                !a.contains(gst::SeekFlags::KEY_UNIT),
                "KEY_UNIT would snap away from the requested time"
            );

            let k = SeekMode::Keyframe.flags();
            assert!(k.contains(gst::SeekFlags::KEY_UNIT));
            assert!(!k.contains(gst::SeekFlags::ACCURATE));
        }
    }

    #[test]
    #[cfg(not(unix))]
    fn native_acceptance_negotiates_real_pixels_without_unix_descriptors() {
        init();
        let caps = sink_caps(FrameAcceptance::Native);
        assert_eq!(caps.size(), 1);
        assert!(!caps.to_string().contains("DMABuf"));
        for format in ["NV12", "P010_10LE", "RGBA", "BGRA", "BGRx", "RGBx"] {
            let decoded = gst::Caps::builder("video/x-raw")
                .field("format", format)
                .build();
            assert!(caps.can_intersect(&decoded), "must accept decoded {format}");
        }
    }

    #[test]
    #[cfg(unix)]
    fn native_acceptance_prefers_dmabuf_and_still_allows_system_memory() {
        init();
        let caps = sink_caps(FrameAcceptance::Native);
        assert!(
            caps.size() >= 2,
            "at least a dmabuf and a system-memory structure"
        );
        // Best-first: the dmabuf structure has to come first or negotiation
        // will settle on system memory even when the GPU could export.
        let first = caps.structure(0).unwrap();
        let feat = caps.features(0).unwrap();
        assert!(
            feat.contains("memory:DMABuf"),
            "first structure must be the dmabuf one, got {first} / {feat}"
        );
        let rest = caps.to_string();
        assert!(
            rest.contains("NV12"),
            "planar system memory remains reachable: {rest}"
        );
        assert!(
            rest.contains("P010_10LE"),
            "10-bit remains reachable: {rest}"
        );
    }
}
