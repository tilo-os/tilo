//! The real decode backend: a GStreamer pipeline that prefers hardware.
//!
//! # What changed and why
//!
//! The previous pipeline was `playbin` with a video sink of
//! `videoconvert ! appsink caps=video/x-raw,format=RGBA`. That caps filter is
//! the expensive part: it *forces* every picture into system-memory RGBA, so a
//! hardware decoder's GPU-resident output is dragged back across the bus and
//! colour-converted on the CPU before anyone has said they want it. Scrubbing
//! then issued a flushing `KEY_UNIT` seek per drawn frame and blipped the
//! pipeline through `PLAYING` to force a frame out, which both landed on the
//! nearest keyframe (up to a GOP away from the requested time) and fired a few
//! milliseconds of audio each time.
//!
//! This backend instead:
//!
//! * ranks hardware decoders above software ones ([`crate::hw`]) and lets
//!   `playbin3` autoplug, then **records the element that was actually
//!   plugged** so hardware decode is an observation, not a claim;
//! * negotiates the best memory the consumer declared it can take —
//!   `memory:DMABuf` first, then planar system memory, and only RGBA when the
//!   consumer says that is all it can paint;
//! * seeks with `ACCURATE` and reads the **preroll** sample while the pipeline
//!   stays in `PAUSED`, so scrubbing lands on the requested frame and never
//!   starts the audio sink;
//! * runs real clock-driven playback when asked to play, rather than
//!   simulating a playhead.

use std::collections::VecDeque;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use gstreamer as gst;
use gstreamer::glib::translate::{FromGlib, IntoGlib};
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;

use crate::dmabuf::{ColorRange, Colorimetry, DmabufImage, DmabufPlane, YuvMatrix};
use crate::event::{MediaEvent, VideoFrame};
use crate::frame::{Frame, FrameAcceptance, FramePayload, PlanarFormat, PlanarImage, PlaneData};
use crate::hw::{DecoderEntry, DecoderKind, HwPolicy, HwReport, rank_for};
use crate::source::Source;

// ---------------------------------------------------------------------------
// libgstallocators — the two entry points needed to read a dmabuf fd out of a
// GstMemory. Bound directly rather than pulling in `gstreamer-allocators`,
// which would add a crate to the workspace lock for three C symbols. Linked
// only under `--features decode`, alongside the rest of the GStreamer stack.
// ---------------------------------------------------------------------------

#[link(name = "gstallocators-1.0")]
unsafe extern "C" {
    /// Whether `mem` was allocated by the dmabuf allocator.
    fn gst_is_dmabuf_memory(mem: *mut gst::ffi::GstMemory) -> gst::glib::ffi::gboolean;
    /// The dmabuf fd behind `mem`. Owned by the memory — the caller must `dup`
    /// it to keep it past the buffer's lifetime.
    fn gst_dmabuf_memory_get_fd(mem: *mut gst::ffi::GstMemory) -> i32;
}

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
        let want = unsafe { gst::Rank::from_glib(rank_for(policy, kind) as i32) };
        if feature.rank() != want {
            feature.set_rank(want);
        }
    }
    tracing::debug!("media: decoder ranks applied for policy {policy:?}");
}

/// Every video decoder in the GStreamer registry, with the rank it currently
/// carries.
///
/// This is what makes "no hardware decode happened" a *checkable* statement
/// rather than an excuse: a caller (or a test) can ask whether a hardware
/// decoder for the stream even exists on this machine before concluding
/// anything about why a software one was chosen.
///
/// Initialises GStreamer if it is not already up; returns an empty list if it
/// cannot be initialised at all.
pub fn available_video_decoders() -> Vec<DecoderEntry> {
    if gst::init().is_err() {
        return Vec::new();
    }
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
                rank: f.rank().into_glib() as u32,
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
        .filter(|d| {
            gst::ElementFactory::find(&d.name)
                .map(|f| f.can_sink_any_caps(&caps))
                .unwrap_or(false)
        })
        .collect()
}

/// The caps an appsink advertises for a given [`FrameAcceptance`].
///
/// Structures are ordered best-first: GStreamer negotiates the first one both
/// sides can satisfy, so a decoder that can export dmabufs will, and one that
/// cannot falls through to planar system memory without a colour conversion.
fn sink_caps(acceptance: FrameAcceptance) -> gst::Caps {
    match acceptance {
        // The compatibility surface: system-memory RGBA, exactly what
        // `VideoFrame` has always carried.
        FrameAcceptance::Rgba => gst::Caps::builder("video/x-raw")
            .field("format", "RGBA")
            .build(),
        FrameAcceptance::Native => gst::Caps::from_str(
            "video/x-raw(memory:DMABuf); \
             video/x-raw, format = (string) { NV12, P010_10LE, RGBA, BGRA, BGRx, RGBx }",
        )
        .expect("static caps string parses"),
    }
}

/// The video sink bin. Under [`FrameAcceptance::Rgba`] a `videoconvert` stands
/// in front of the sink so any decoder output reaches RGBA; under
/// [`FrameAcceptance::Native`] the sink is bare and negotiation decides.
fn build_video_sink(acceptance: FrameAcceptance) -> Option<(gst::Element, gst_app::AppSink)> {
    let appsink = gst_app::AppSink::builder()
        .name("qos_vsink")
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
            let bin = gst::Bin::builder().name("qos_vbin").build();
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

    volume: f32,
    muted: bool,
    policy: HwPolicy,

    /// Monotonic count of pictures stored since this pipeline was built.
    /// Paired with `frames_at_seek`, this is how a caller distinguishes "the
    /// frame for the position I just asked for" from "the frame that was
    /// already on screen".
    frames_stored: u64,
    /// `frames_stored` at the moment the last seek was accepted.
    frames_at_seek: u64,
}

impl GstState {
    /// Build the pipeline with no source yet. `None` when GStreamer cannot
    /// initialise or the sink chain cannot be built, so the engine falls back
    /// to the synthesized backend.
    pub fn new(acceptance: FrameAcceptance, policy: HwPolicy) -> Option<Self> {
        gst::init().ok()?;
        // Ranks are re-applied per pipeline rather than once per process.
        // GStreamer's registry ranks are global, so the most recently built
        // engine's policy is the one in force — but re-applying makes the
        // policy a property of the engine's configuration rather than of
        // whichever code happened to touch the registry first, which is what
        // lets a caller (and a test) actually choose it.
        apply_decoder_ranks(policy);

        // `playbin3` drives `decodebin3`, which does per-stream decoder
        // selection and gapless stream switching. Fall back to `playbin` on a
        // build that predates it.
        let playbin = gst::ElementFactory::make("playbin3")
            .build()
            .or_else(|_| gst::ElementFactory::make("playbin").build())
            .ok()?;

        let (video_sink, appsink) = build_video_sink(acceptance)?;
        playbin.set_property("video-sink", &video_sink);

        // Audio through a real sink so playback is audible, with an explicit
        // `volume` element: `playbin`'s own volume property re-negotiates the
        // stream, while the element is a plain gain stage that can be moved
        // per-source without disturbing the pipeline.
        if let Ok(audio_bin) = gst::parse::bin_from_description(
            "audioconvert ! audioresample ! volume name=qos_gain ! autoaudiosink name=qos_asink",
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
                    if let Ok(mut o) = observed.lock() {
                        if o.report.note_element(&name) {
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
            volume: 0.8,
            muted: false,
            policy,
            frames_stored: 0,
            frames_at_seek: 0,
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
        self.frames_stored = 0;
        self.frames_at_seek = 0;
        if let Ok(mut o) = self.observed.lock() {
            o.report = HwReport::default();
        }

        // Another engine may have been built since, leaving the registry on
        // its policy. Re-assert ours before the decoder is autoplugged.
        apply_decoder_ranks(self.policy);
        self.set_drop_policy(false);
        self.playbin.set_property("uri", source.to_uri());
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
        let settled = matches!(
            self.playbin
                .state(gst::ClockTime::from_mseconds(timeout.as_millis() as u64))
                .0,
            Ok(gst::StateChangeSuccess::Success) | Ok(gst::StateChangeSuccess::NoPreroll)
        );
        if settled {
            self.seek_in_flight = false;
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
        let pos = gst::ClockTime::from_mseconds(ms);
        match self.playbin.seek_simple(mode.flags(), pos) {
            Ok(()) => {
                self.seek_in_flight = true;
                self.seek_target_ms = Some(ms);
                // Anything already held belongs to the *previous* position.
                // Record the watermark rather than clearing the frame: the
                // preview keeps painting the old picture (no flicker) while
                // `frame_is_post_seek` reports, truthfully, that it is stale.
                self.frames_at_seek = self.frames_stored;
                true
            }
            Err(e) => {
                // A pipeline that has not finished prerolling refuses seeks.
                // That is expected right after `open`, and the caller retries —
                // but it must know, or it waits forever for a picture nobody
                // ever asked for.
                tracing::debug!("media: seek to {ms}ms not accepted yet: {e}");
                false
            }
        }
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
        let level = if self.muted { 0.0 } else { self.volume as f64 };
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
        sink.downcast::<gst::Bin>().ok()?.by_name("qos_gain")
    }

    /// The audio sink element, the entry point for routing or monitoring the
    /// output (its `device` property, a PipeWire target node, a tee for a
    /// meter). `None` when the audio bin could not be built.
    pub fn audio_sink(&self) -> Option<gst::Element> {
        let sink = self
            .playbin
            .property::<Option<gst::Element>>("audio-sink")?;
        sink.downcast::<gst::Bin>().ok()?.by_name("qos_asink")
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
            .map(|p| p.mseconds())
            .or(self.seek_target_ms)
            .or(self.last_position)
            .or_else(|| self.latest.as_ref().map(|f| f.pts_ms))
            .unwrap_or(0)
    }

    pub fn duration_ms(&self) -> Option<u64> {
        self.playbin
            .query_duration::<gst::ClockTime>()
            .map(|d| d.mseconds())
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
        self.frames_stored > self.frames_at_seek
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
        if !self.playing || self.latest.is_none() {
            if let Some(sample) = self.appsink.try_pull_preroll(gst::ClockTime::ZERO) {
                if let Some(frame) = frame_from_sample(&sample, self.acceptance) {
                    self.record_caps(&sample);
                    self.store(frame);
                    got = true;
                }
            }
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
        self.frames_stored += 1;
        self.latest_rgba = match frame.payload {
            FramePayload::Rgba(_) => frame.to_video_frame(),
            _ => None,
        };
        self.latest = Some(frame);
    }

    /// Record the caps the sink negotiated — the proof of which memory the
    /// frames actually arrive in.
    fn record_caps(&self, sample: &gst::Sample) {
        if let Some(caps) = sample.caps() {
            if let Ok(mut o) = self.observed.lock() {
                if o.report.negotiated_caps.is_none() {
                    o.report.negotiated_caps = Some(caps.to_string());
                }
            }
        }
    }

    pub fn poll(&mut self) -> Vec<MediaEvent> {
        let mut events: Vec<MediaEvent> = self.pending.drain(..).collect();

        if !self.duration_announced {
            if let Some(ms) = self.duration_ms() {
                events.push(MediaEvent::DurationKnown { ms });
                self.duration_announced = true;
            }
        }

        while let Some(msg) = self.bus.pop() {
            use gst::MessageView;
            match msg.view() {
                MessageView::Buffering(b) => {
                    let pct = b.percent().clamp(0, 100) as u8;
                    self.buffering = pct;
                    if self.last_buffering != Some(pct) {
                        self.last_buffering = Some(pct);
                        events.push(MediaEvent::Buffering { percent: pct });
                    }
                }
                MessageView::AsyncDone(_) => {
                    // The flushing seek finished; the sink now holds the frame
                    // for the requested time.
                    self.seek_in_flight = false;
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

        self.drain_samples();

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

// ---------------------------------------------------------------------------
// Sample → Frame
// ---------------------------------------------------------------------------

/// Turn one appsink sample into a [`Frame`] in whatever memory it arrived in.
fn frame_from_sample(sample: &gst::Sample, acceptance: FrameAcceptance) -> Option<Frame> {
    let caps = sample.caps()?;
    let buffer = sample.buffer()?;
    let pts_ms = buffer.pts().map(|p| p.mseconds()).unwrap_or(0);

    // A dmabuf sample carries `memory:DMABuf` in its caps features and a
    // `DMA_DRM` format described by `drm-format`.
    if caps
        .features(0)
        .map(|f| f.contains("memory:DMABuf"))
        .unwrap_or(false)
    {
        if acceptance == FrameAcceptance::Native {
            if let Some(img) = dmabuf_from_sample(sample, caps) {
                return Some(Frame {
                    width: img.width,
                    height: img.height,
                    pts_ms,
                    payload: FramePayload::Dmabuf(Arc::new(img)),
                });
            }
            tracing::warn!("media: dmabuf sample could not be described; dropping frame");
            return None;
        }
        // An RGBA-only consumer cannot be handed a dmabuf, and mapping one
        // here would be a silent stall. Negotiation should have prevented
        // this; say so rather than pretending.
        tracing::warn!("media: dmabuf negotiated for an RGBA-only consumer; dropping frame");
        return None;
    }

    let info = gst_video::VideoInfo::from_caps(caps).ok()?;
    let vframe = gst_video::VideoFrameRef::from_buffer_ref_readable(buffer, &info).ok()?;
    let colorimetry = colorimetry_from_info(&info);

    match info.format() {
        gst_video::VideoFormat::Nv12 | gst_video::VideoFormat::P01010le => {
            let format = if info.format() == gst_video::VideoFormat::Nv12 {
                PlanarFormat::Nv12
            } else {
                PlanarFormat::P010
            };
            let planes = (0..2)
                .map(|i| {
                    let data = vframe.plane_data(i).ok()?;
                    Some(PlaneData {
                        bytes: Arc::new(data.to_vec()),
                        stride: info.stride()[i as usize] as u32,
                        width: if i == 0 {
                            info.width()
                        } else {
                            info.width() / 2
                        },
                        height: if i == 0 {
                            info.height()
                        } else {
                            info.height() / 2
                        },
                    })
                })
                .collect::<Option<Vec<_>>>()?;
            Some(Frame {
                width: info.width(),
                height: info.height(),
                pts_ms,
                payload: FramePayload::Planar(Arc::new(PlanarImage {
                    format,
                    planes,
                    colorimetry,
                })),
            })
        }
        _ => {
            // Packed 8-bit formats: repack to tight RGBA, honouring stride.
            let rgba = packed_to_rgba(&vframe, &info)?;
            Some(Frame {
                width: info.width(),
                height: info.height(),
                pts_ms,
                payload: FramePayload::Rgba(Arc::new(rgba)),
            })
        }
    }
}

/// Repack a packed 8-bit sample (RGBA / BGRA / BGRx / RGBx) into tight RGBA8.
fn packed_to_rgba(
    vframe: &gst_video::VideoFrameRef<&gst::BufferRef>,
    info: &gst_video::VideoInfo,
) -> Option<Vec<u8>> {
    let w = info.width() as usize;
    let h = info.height() as usize;
    let stride = info.stride()[0] as usize;
    let data = vframe.plane_data(0).ok()?;
    let row = w * 4;
    if stride < row || data.len() < stride * h {
        return None;
    }
    // Which source byte feeds each destination channel.
    let swizzle: [usize; 4] = match info.format() {
        gst_video::VideoFormat::Rgba | gst_video::VideoFormat::Rgbx => [0, 1, 2, 3],
        gst_video::VideoFormat::Bgra | gst_video::VideoFormat::Bgrx => [2, 1, 0, 3],
        _ => [0, 1, 2, 3],
    };
    let opaque = matches!(
        info.format(),
        gst_video::VideoFormat::Rgbx | gst_video::VideoFormat::Bgrx
    );
    let mut out = vec![0u8; row * h];
    for y in 0..h {
        let src = &data[y * stride..y * stride + row];
        let dst = &mut out[y * row..(y + 1) * row];
        if swizzle == [0, 1, 2, 3] && !opaque {
            dst.copy_from_slice(src);
        } else {
            for x in 0..w {
                let s = &src[x * 4..x * 4 + 4];
                let d = &mut dst[x * 4..x * 4 + 4];
                d[0] = s[swizzle[0]];
                d[1] = s[swizzle[1]];
                d[2] = s[swizzle[2]];
                d[3] = if opaque { 255 } else { s[swizzle[3]] };
            }
        }
    }
    Some(out)
}

/// The colour convention the caps declare, defaulting to BT.709 limited —
/// what HD video carries when it says nothing.
fn colorimetry_from_info(info: &gst_video::VideoInfo) -> Colorimetry {
    let c = info.colorimetry();
    let matrix = match c.matrix() {
        gst_video::VideoColorMatrix::Bt601 => YuvMatrix::Bt601,
        gst_video::VideoColorMatrix::Bt709 => YuvMatrix::Bt709,
        gst_video::VideoColorMatrix::Bt2020 => YuvMatrix::Bt2020,
        gst_video::VideoColorMatrix::Rgb => YuvMatrix::Identity,
        _ => YuvMatrix::Bt709,
    };
    let range = match c.range() {
        gst_video::VideoColorRange::Range0_255 => ColorRange::Full,
        _ => ColorRange::Limited,
    };
    Colorimetry { matrix, range }
}

/// Describe a `memory:DMABuf` sample: pull the fd out of each `GstMemory` and
/// read the plane layout from the buffer's `GstVideoMeta` (which the sink's
/// `propose_allocation` answer is what makes available).
fn dmabuf_from_sample(sample: &gst::Sample, caps: &gst::CapsRef) -> Option<DmabufImage> {
    let buffer = sample.buffer()?;
    let drm = gst_video::VideoInfoDmaDrm::from_caps(caps).ok()?;

    // `DMA_DRM` caps describe an opaque buffer: the `VideoInfo` inside a
    // `VideoInfoDmaDrm` carries the *placeholder* DMA_DRM format, which has
    // zero planes and no strides. Translating it back to the real format
    // (NV12, P010, …) is what yields the plane count and layout — without
    // this the frame is described as having no planes at all, and every
    // import then fails for a reason that has nothing to do with the buffer.
    let resolved = drm.to_video_info().ok();
    let info: &gst_video::VideoInfo = resolved.as_ref().unwrap_or(&drm);
    let width = info.width();
    let height = info.height();
    let n_planes = info.n_planes() as usize;
    if n_planes == 0 {
        tracing::warn!(
            "media: dmabuf caps {} resolved to zero planes; cannot describe the frame",
            caps
        );
        return None;
    }

    // Plane geometry: prefer the buffer's VideoMeta (authoritative for the
    // actual allocation), fall back to the caps-derived info.
    let meta = buffer.meta::<gst_video::VideoMeta>();
    let (offsets, strides): (Vec<u32>, Vec<u32>) = match &meta {
        Some(m) => (
            m.offset()
                .iter()
                .take(n_planes)
                .map(|o| *o as u32)
                .collect(),
            m.stride()
                .iter()
                .take(n_planes)
                .map(|s| *s as u32)
                .collect(),
        ),
        None => (
            (0..n_planes).map(|i| info.offset()[i] as u32).collect(),
            (0..n_planes).map(|i| info.stride()[i] as u32).collect(),
        ),
    };
    if offsets.len() < n_planes || strides.len() < n_planes {
        tracing::warn!("media: dmabuf plane geometry incomplete; cannot describe frame");
        return None;
    }

    // One GstMemory may back several planes (offsets into one fd), or each
    // plane may have its own. Map plane → memory by GStreamer's own rule.
    let n_mem = buffer.n_memory();
    let mut planes = Vec::with_capacity(n_planes);
    for i in 0..n_planes {
        let mem_idx = if n_mem == 1 { 0 } else { i.min(n_mem - 1) };
        let mem = buffer.peek_memory(mem_idx);
        // Safety: `mem` is a live `&MemoryRef` borrowed from the buffer for
        // the duration of this call; both C entry points only read from it.
        let raw = mem.as_ptr() as *mut gst::ffi::GstMemory;
        let is_dmabuf = unsafe { gst_is_dmabuf_memory(raw) } != 0;
        if !is_dmabuf {
            tracing::warn!("media: caps claimed memory:DMABuf but memory {mem_idx} is not one");
            return None;
        }
        let raw_fd = unsafe { gst_dmabuf_memory_get_fd(raw) };
        if raw_fd < 0 {
            tracing::warn!("media: dmabuf memory {mem_idx} has no fd");
            return None;
        }
        // The fd belongs to the GstMemory, which is released when the sample
        // drops. Duplicate it so the frame can outlive the sample.
        let borrowed = unsafe { std::os::fd::BorrowedFd::borrow_raw(raw_fd) };
        let fd = borrowed.try_clone_to_owned().ok()?;
        planes.push(DmabufPlane {
            fd,
            offset: offsets[i],
            stride: strides[i],
        });
    }

    Some(DmabufImage {
        planes,
        fourcc: drm.fourcc(),
        // GStreamer reports a linear buffer as modifier 0, which is exactly
        // DRM_FORMAT_MOD_LINEAR — keep it rather than reinterpreting it as
        // "unstated", or a genuinely linear buffer becomes indistinguishable
        // from one whose producer declined to say.
        modifier: drm.modifier(),
        width,
        height,
        colorimetry: colorimetry_from_info(info),
    })
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
    fn rgba_acceptance_asks_only_for_system_memory_rgba() {
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

    #[test]
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

    #[test]
    fn accurate_seek_carries_the_accurate_flag_and_keyframe_seek_does_not() {
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
