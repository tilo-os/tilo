//! The `GStreamer` decode backend: `playbin3` with hardware decoders ranked
//! first, sink caps that take the best memory the consumer can paint, accurate
//! seeks read from the preroll while the pipeline stays paused, and clock-driven
//! playback.

use std::collections::{BTreeSet, VecDeque};
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
use crate::hw::fallback::{self, Direction};
use crate::hw::v4l2;
use crate::hw::{DecoderEntry, DecoderKind, HwPolicy, HwReport, effective_rank};
use crate::source::Source;
use crate::{MediaChapter, MediaTrack};

/// Raise every decoder in the registry to the rank [`HwPolicy`] assigns it. Once
/// per process, right after `gst::init`: a build whose `va*` elements ship at rank
/// `NONE` never autoplugs them otherwise.
///
/// `refused` names hardware decoders that already failed on this source; they
/// drop to rank `NONE`, so a reopened pipeline plugs the next candidate.
fn apply_decoder_ranks(policy: HwPolicy, refused: &BTreeSet<String>) {
    static SHIPPED: Mutex<std::collections::BTreeMap<String, u32>> =
        Mutex::new(std::collections::BTreeMap::new());
    let registry = gst::Registry::get();
    let Ok(mut shipped) = SHIPPED.lock() else {
        return;
    };
    for feature in registry.features(gst::ElementFactory::static_type()) {
        let name = feature.name().to_string();
        if !crate::hw::looks_like_video_decoder(&name) {
            continue;
        }
        let kind = DecoderKind::classify(&name);
        let original = *shipped
            .entry(name.clone())
            .or_insert_with(|| u32::try_from(feature.rank().into_glib()).unwrap_or(0));
        // `Rank` is a transparent wrapper over the C `gint` rank and any value is
        // meaningful. The scale is small (`GST_RANK_PRIMARY` is 256), so the conversion is
        // exact; a rank that did not fit is left as high as possible.
        let rank = if refused.contains(&name) {
            0
        } else {
            i32::try_from(effective_rank(policy, kind, original)).unwrap_or(i32::MAX)
        };
        let want = unsafe { gst::Rank::from_glib(rank) };
        if feature.rank() != want {
            feature.set_rank(want);
        }
    }
    tracing::debug!("media: decoder ranks applied for policy {policy:?}, refused {refused:?}");
}

/// The hardware decoders in the registry for `media_type`, by factory name. Reads
/// the registry as it stands, so it is cheap enough for a streaming thread.
fn hardware_decoders_for(media_type: &str) -> Vec<String> {
    gst::Registry::get()
        .features(gst::ElementFactory::static_type())
        .into_iter()
        .map(|f| f.name().to_string())
        .filter(|name| {
            crate::hw::looks_like_video_decoder(name)
                && DecoderKind::classify(name).is_hardware()
                && v4l2::element_media_type(name) == Some(media_type)
        })
        .collect()
}

fn decoder_input(element: &gst::Element) -> (Option<&'static str>, Option<(u32, u32)>) {
    let factory = element.factory().map(|f| f.name().to_string());
    let caps = element
        .static_pad("sink")
        .and_then(|pad| pad.current_caps());
    let structure = caps.as_ref().and_then(|c| c.structure(0));
    let media_type = factory
        .as_deref()
        .and_then(v4l2::element_media_type)
        .or_else(|| structure.and_then(|st| v4l2::element_media_type(st.name())));
    let size = structure.and_then(|st| {
        let w = u32::try_from(st.get::<i32>("width").ok()?).ok()?;
        let h = u32::try_from(st.get::<i32>("height").ok()?).ok()?;
        Some((w, h))
    });
    (media_type, size)
}

/// Every video decoder in the registry with the rank it carries now. This makes
/// "no hardware decode happened" a checkable statement: a caller can ask whether a
/// hardware decoder for the stream exists here at all.
///
/// Initialises `GStreamer` if it is not up; empty when it cannot be.
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
                rank: u32::try_from(f.rank().into_glib()).unwrap_or(0),
                name,
            })
        })
        .collect();
    out.sort_by(|a, b| b.rank.cmp(&a.rank).then_with(|| a.name.cmp(&b.name)));
    out
}

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

/// The caps an appsink advertises for a [`FrameAcceptance`], best first:
/// `GStreamer` negotiates the first structure both sides satisfy, so a decoder
/// that can export dmabufs will, and one that cannot falls through to planar
/// system memory without a colour conversion.
fn sink_caps(acceptance: FrameAcceptance) -> gst::Caps {
    #[cfg(unix)]
    const NATIVE_CAPS: &str = "video/x-raw(memory:DMABuf); video/x-raw, format = (string) { NV12, P010_10LE, RGBA, BGRA, BGRx, RGBx }";
    // DMA-BUF is a Unix fd protocol, not a Windows shared-texture handle.
    #[cfg(not(unix))]
    const NATIVE_CAPS: &str =
        "video/x-raw, format = (string) { NV12, P010_10LE, RGBA, BGRA, BGRx, RGBx }";
    match acceptance {
        FrameAcceptance::Rgba => gst::Caps::builder("video/x-raw")
            .field("format", "RGBA")
            .build(),
        FrameAcceptance::Native => gst::Caps::from_str(NATIVE_CAPS).unwrap_or_else(|_| {
            // Unreachable: the argument is a string literal. A parse failure costs the
            // zero-copy path, not the process.
            gst::Caps::builder("video/x-raw")
                .field("format", "RGBA")
                .build()
        }),
    }
}

/// The video sink bin. Under [`FrameAcceptance::Rgba`] a `videoconvert` in front
/// of the sink lets any decoder output reach RGBA; under [`FrameAcceptance::Native`]
/// the sink is bare and negotiation decides.
///
/// The RGBA bin also returns its copy stage: an `identity` that, once a V4L2
/// decoder is plugged, drops the allocation query so the decoder copies each
/// picture into cached memory before the conversion reads it. V4L2 buffers are
/// mapped write-combined on ARM chips, and converting them in place is
/// pathological: measured on an RK3566, 1080p H.264 from the Hantro decoder
/// reached 2.8 fps in place and 16 fps after one sequential copy.
fn build_video_sink(
    acceptance: FrameAcceptance,
) -> Option<(gst::Element, gst_app::AppSink, Option<gst::Element>)> {
    let appsink = gst_app::AppSink::builder()
        .name("tilo_vsink")
        .caps(&sink_caps(acceptance))
        // `sync=true` paces frames to their PTS during playback. Scrubbing reads the
        // preroll, which is not paced.
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
            Some((element, appsink, None))
        }
        FrameAcceptance::Rgba => {
            let bin = gst::Bin::builder().name("tilo_vbin").build();
            let copy = gst::ElementFactory::make("identity")
                .name("tilo_hw_copy")
                .property("drop-allocation", false)
                .build()
                .ok()?;
            let convert = gst::ElementFactory::make("videoconvert")
                .property("n-threads", 0u32)
                .build()
                .ok()?;
            let sink_el = appsink.clone().upcast::<gst::Element>();
            bin.add_many([&copy, &convert, &sink_el]).ok()?;
            gst::Element::link_many([&copy, &convert, &sink_el]).ok()?;
            let pad = copy.static_pad("sink")?;
            let ghost = gst::GhostPad::with_target(&pad).ok()?;
            bin.add_pad(&ghost).ok()?;
            Some((bin.upcast::<gst::Element>(), appsink, Some(copy)))
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SeekMode {
    Accurate,
    // Snap to the nearest keyframe: cheap, up to a GOP away from the requested time.
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

#[derive(Default)]
struct Observed {
    report: HwReport,
    refused: Vec<(String, String)>,
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
    governed_output: Option<Arc<crate::audio_output::gst::Bridge>>,
    audio_buffers: Arc<std::sync::atomic::AtomicU64>,
    draining_output: bool,
    output_error_reported: bool,

    latest: Option<Frame>,
    // [`VideoFrame`] view of `latest`, materialized on demand and cached so repeated
    // calls while paused cost nothing.
    latest_rgba: Option<VideoFrame>,

    playing: bool,
    duration_announced: bool,
    last_position: Option<u64>,
    last_buffering: Option<u8>,
    buffering: u8,
    pending: VecDeque<MediaEvent>,

    seek_in_flight: bool,
    // The position the last seek asked for, so `position_ms` reports it rather than a
    // stale query result while the seek settles.
    seek_target_ms: Option<u64>,
    deferred_seek: Option<(u64, SeekMode)>,

    volume: f32,
    muted: bool,
    rate: f32,
    policy: HwPolicy,

    // Pictures stored since this pipeline was built. With `frames_at_seek` it tells
    // the frame for the position just asked for from the one already on screen.
    frames_stored: u64,
    frames_at_seek: u64,
    // The flattened container TOC. The bus publishes it asynchronously after
    // discovery, so callers poll it without blocking decode.
    chapters: Vec<MediaChapter>,

    // The URI open now, so a refused hardware decoder can be retried past on the same
    // source.
    uri: Option<String>,
    refused: BTreeSet<String>,
    resume: Option<(u64, bool)>,
    // Why the source is not in hardware: the refused decoder's reason and its media
    // type.
    refusal: Option<(&'static str, String)>,
    missing_decoder: Option<String>,
    copy_stage: Option<gst::Element>,
}

/// Do the process-wide part of a first pipeline on a thread of its own:
/// `GStreamer` init and plugin registry, and loading the plugins a playback
/// pipeline is built from — hundreds of milliseconds that otherwise fall on the
/// thread building the first engine. Everything here is global and idempotent, so
/// an engine built while it runs waits for the same work. Once per process.
pub(crate) fn prepare_in_background() {
    static STARTED: std::sync::Once = std::sync::Once::new();
    STARTED.call_once(|| {
        let _ = std::thread::Builder::new()
            .name("media-prepare".into())
            .spawn(|| {
                if gst::init().is_err() {
                    return;
                }
                crate::extensions::refresh_registry();
                for name in [
                    "playbin",
                    "appsink",
                    "videoconvert",
                    "audioconvert",
                    "audioresample",
                    "volume",
                    "autoaudiosink",
                ] {
                    if let Some(factory) = gst::ElementFactory::find(name) {
                        let _ = factory.load();
                    }
                }
            });
    });
}

/// Why neither `playbin` nor `playbin3` could be made: `GStreamer` looks for
/// plugins beside the `libgstreamer` it was loaded from unless told otherwise, so
/// name where it looked and what it found there.
fn no_playback_element() -> String {
    let plugins = gst::Registry::get().plugins().len();
    let searched = std::env::var("GST_PLUGIN_SYSTEM_PATH_1_0")
        .unwrap_or_else(|_| "the directory beside libgstreamer".into());
    format!("GStreamer has no playback element: {plugins} plugins registered from {searched}")
}

impl GstState {
    /// Build the pipeline with no source yet.
    ///
    /// # Errors
    ///
    /// Why no pipeline could be built: `GStreamer` would not initialise, its registry
    /// has no playback element (no plugins were found), or the sink chain could not be
    /// assembled.
    pub fn new(
        acceptance: FrameAcceptance,
        policy: HwPolicy,
        audio_output: bool,
        video_output: bool,
    ) -> Result<Self, String> {
        gst::init().map_err(|error| format!("GStreamer could not start: {error}"))?;
        // Pick up plugins a codec-pack merge added (or an unmerge removed) before ranking.
        crate::extensions::refresh_registry();
        // Re-applied per pipeline rather than once per process: the registry ranks are
        // global, but this makes the policy part of an engine's configuration instead of
        // a property of whichever code touched the registry first.
        apply_decoder_ranks(policy, &BTreeSet::new());

        let playbin = gst::ElementFactory::make("playbin")
            .build()
            .or_else(|_| gst::ElementFactory::make("playbin3").build())
            .map_err(|_| no_playback_element())?;

        let (video_sink, appsink, copy_stage) = build_video_sink(acceptance)
            .ok_or("GStreamer could not build the video output (appsink, videoconvert)")?;
        playbin.set_property("video-sink", &video_sink);

        // Audio through a real sink so playback is audible, with an explicit `volume`
        // element: `playbin`'s own volume property re-negotiates the stream, while a gain
        // stage can be moved per source. An engine that never plays sound opens no audio
        // device.
        let mut governed_output = None;
        if !video_output {
            // Sound only: the picture the source may also carry is never decoded.
            playbin.set_property_from_str("flags", "audio");
        }
        if !audio_output {
            playbin.set_property_from_str("flags", "video+text+deinterlace+soft-colorbalance");
        } else if crate::audio_output::is_governed() {
            let (audio_bin, output) = crate::audio_output::gst::sink_bin("tilo_asink", "tilo_gain")
                .map_err(|error| format!("the audio output could not be built: {error}"))?;
            playbin.set_property("audio-sink", &audio_bin);
            governed_output = Some(output);
        } else if let Ok(audio_bin) = gst::parse::bin_from_description(
            "audioconvert ! audioresample ! volume name=tilo_gain ! autoaudiosink name=tilo_asink",
            true,
        ) {
            playbin.set_property("audio-sink", &audio_bin);
        }
        playbin.set_property("mute", false);
        playbin.set_property("volume", 0.8f64);
        let audio_buffers = Arc::new(std::sync::atomic::AtomicU64::new(0));
        if let Some(pad) = playbin
            .property::<Option<gst::Element>>("audio-sink")
            .and_then(|sink| sink.downcast::<gst::Bin>().ok()?.by_name("tilo_asink"))
            .and_then(|sink| sink.static_pad("sink"))
        {
            let audio_buffers = audio_buffers.clone();
            let _ = pad.add_probe(gst::PadProbeType::BUFFER, move |_, _| {
                audio_buffers.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                gst::PadProbeReturn::Ok
            });
        }

        let observed = Arc::new(Mutex::new(Observed::default()));

        // Watch every element the pipeline instantiates, however deeply nested, and record
        // the decoder: the element GStreamer chose is the evidence that hardware decode
        // engaged.
        {
            let observed = observed.clone();
            let copy_stage = copy_stage.clone();
            playbin.connect("deep-element-added", false, move |vals| {
                if let Some(element) = vals.get(2).and_then(|v| v.get::<gst::Element>().ok()) {
                    on_element_added(&element, &observed, copy_stage.as_ref(), policy, acceptance);
                }
                None
            });
        }

        let bus = playbin
            .bus()
            .ok_or("the playback pipeline has no message bus")?;
        Ok(Self {
            playbin,
            appsink,
            bus,
            acceptance,
            observed,
            governed_output,
            audio_buffers,
            draining_output: false,
            output_error_reported: false,
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
            uri: None,
            refused: BTreeSet::new(),
            resume: None,
            refusal: None,
            missing_decoder: None,
            copy_stage,
        })
    }

    pub fn acceptance(&self) -> FrameAcceptance {
        self.acceptance
    }

    pub fn policy(&self) -> HwPolicy {
        self.policy
    }

    // What the pipeline actually plugged. Empty until the first frame has prerolled.
    pub fn hw_report(&self) -> HwReport {
        self.observed
            .lock()
            .map(|o| o.report.clone())
            .unwrap_or_default()
    }

    pub fn open(&mut self, source: &Source, volume: f32) {
        let _ = self.playbin.set_state(gst::State::Null);
        if let Some(output) = &self.governed_output {
            output.reset();
            output.pause(true);
        }
        self.latest = None;
        self.latest_rgba = None;
        self.playing = false;
        self.duration_announced = false;
        self.last_position = None;
        self.last_buffering = None;
        self.buffering = 100;
        self.pending.clear();
        self.draining_output = false;
        self.output_error_reported = false;
        self.seek_in_flight = false;
        self.seek_target_ms = None;
        self.deferred_seek = None;
        self.frames_stored = 0;
        self.frames_at_seek = 0;
        self.audio_buffers
            .store(0, std::sync::atomic::Ordering::Relaxed);
        self.chapters.clear();
        self.resume = None;
        let uri = match source.to_uri() {
            Ok(uri) => uri,
            Err(error) => {
                self.pending.push_back(MediaEvent::Error(error.to_string()));
                return;
            }
        };
        self.missing_decoder = None;
        if self.uri.as_deref() != Some(uri.as_str()) {
            self.refused.clear();
            self.refusal = None;
            if let Ok(mut o) = self.observed.lock() {
                o.refused.clear();
            }
        }
        if let Ok(mut o) = self.observed.lock() {
            o.report = HwReport::default();
        }
        if let Some(copy) = &self.copy_stage {
            copy.set_property("drop-allocation", false);
        }

        crate::extensions::refresh_registry();
        apply_decoder_ranks(self.policy, &self.refused);
        self.set_drop_policy(false);
        self.playbin.set_property("uri", &uri);
        self.uri = Some(uri);
        self.volume = volume.clamp(0.0, 1.0);
        self.apply_gain();
        // Preroll: a first frame and the duration arrive without entering PLAYING, so
        // opening a clip is silent.
        let _ = self.playbin.set_state(gst::State::Paused);
    }

    /// Block until the pipeline finishes its current asynchronous state change or
    /// `timeout` elapses; `true` if it settled. A flushing seek's new preroll is
    /// guaranteed only once the state change completes.
    pub fn wait_settled(&mut self, timeout: std::time::Duration) -> bool {
        // A timeout that does not fit `u64` waits effectively forever, which is what
        // saturating gives.
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

    /// Whether the sink discards buffers when its queue fills. The two modes want
    /// opposite behaviour, and getting it wrong is a silent two-frame error, not a
    /// stall: after a flushing seek the decoder races ahead, so `drop=true` discards
    /// the oldest buffer — including the frame seeked to — and serves a later one,
    /// an offset that looks like an inaccurate seek, while `drop=false` blocks the
    /// decoder and keeps the frame. While playing, dropping is right, so a slow
    /// consumer never lags the clock.
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
        if let Some(output) = &self.governed_output {
            output.pause(!playing);
        }
        if playing {
            self.seek_target_ms = None;
        }
    }

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
        // `playbin3` and older `playbin` do not all expose the same stream-selection
        // properties: a missing optional capability is an empty enumeration, not a panic.
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
                // The stream-id the demuxer emits is the only safe identity to publish: an array
                // position could target another stream after renegotiation.
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

    /// Seek to `ms`. [`SeekMode::Accurate`] decodes forward from the preceding
    /// keyframe, so the frame presented is the one at `ms`. The pipeline keeps its
    /// state: a paused one re-prerolls and the new frame arrives on the preroll path,
    /// so scrubbing never starts the audio sink.
    pub fn seek(&mut self, ms: u64, mode: SeekMode) -> bool {
        // A flushing seek while the first preroll is still negotiating fails the V4L2
        // stateless decoders' allocation query; they conclude the sink cannot take
        // dmabufs and never preroll, with no error on the bus. Hold the seek instead.
        if self.frames_stored == 0
            && matches!(
                self.playbin.state(gst::ClockTime::ZERO).0,
                Ok(gst::StateChangeSuccess::Async)
            )
        {
            self.deferred_seek = Some((ms, mode));
            self.seek_in_flight = true;
            self.seek_target_ms = Some(ms);
            // The preroll picture about to land is not the one asked for.
            self.frames_at_seek = self.frames_stored.saturating_add(1);
            return true;
        }
        // Before the first preroll, decodebin may still be autoplugging, and a flushing
        // seek accepted in that window can leave the decoder's caps unnegotiated, so no
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

    fn apply_deferred_seek(&mut self) {
        let Some((ms, mode)) = self.deferred_seek else {
            return;
        };
        let (_, current, _) = self.playbin.state(gst::ClockTime::ZERO);
        if self.frames_stored == 0
            && matches!(
                self.playbin.state(gst::ClockTime::ZERO).0,
                Ok(gst::StateChangeSuccess::Async)
            )
        {
            return;
        }
        if current >= gst::State::Paused && self.issue_seek(ms, mode) {
            self.deferred_seek = None;
        }
    }

    fn issue_seek(&mut self, ms: u64, mode: SeekMode) -> bool {
        if let Some(output) = &self.governed_output {
            output.flush();
        }

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
        // Anything held belongs to the previous position. Record the watermark rather
        // than clearing the frame: the preview keeps painting the old picture while
        // `frame_is_post_seek` reports it stale.
        self.frames_at_seek = self.frames_stored;
    }

    // Volume in `0.0..=1.0`, on the gain element so the change does not disturb the
    // negotiated stream.
    pub fn set_volume(&mut self, v: f32) {
        self.volume = v.clamp(0.0, 1.0);
        self.apply_gain();
    }

    pub fn volume(&self) -> f32 {
        self.volume
    }

    pub fn set_muted(&mut self, muted: bool) {
        self.muted = muted;
        self.apply_gain();
    }

    pub fn is_muted(&self) -> bool {
        self.muted
    }

    // Push volume and mute to the `volume` element, or to playbin's own property when
    // the audio bin could not be built.
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

    fn gain_element(&self) -> Option<gst::Element> {
        let sink = self
            .playbin
            .property::<Option<gst::Element>>("audio-sink")?;
        sink.downcast::<gst::Bin>().ok()?.by_name("tilo_gain")
    }

    /// Decoded audio the output has rendered since the last open: the device's
    /// progress for a governed output, else the buffers that reached the sink.
    pub fn audio_buffers_rendered(&self) -> u64 {
        match &self.governed_output {
            Some(output) => output.observation().rendered_frames,
            None => self
                .audio_buffers
                .load(std::sync::atomic::Ordering::Relaxed),
        }
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
        // While a seek is settling the pipeline still reports the old position; the
        // target is the honest answer until the new frame lands.
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

    /// The newest picture in its native memory. May belong to a previous position
    /// while a seek is in flight — see [`Self::frame_is_post_seek`].
    pub fn latest(&self) -> Option<&Frame> {
        self.latest.as_ref()
    }

    /// Whether the held picture was decoded after the most recent seek. Without it,
    /// "a frame exists" is satisfied by the frame already on screen. The frame is not
    /// cleared on seek — the preview keeps painting the old one, so scrubbing does not
    /// flicker — so staleness is reported rather than inferred from absence.
    pub fn frame_is_post_seek(&self) -> bool {
        // The first preroll is not the picture a deferred seek asked for.
        self.deferred_seek.is_none() && self.frames_stored > self.frames_at_seek
    }

    /// The newest picture as system-memory RGBA. Under [`FrameAcceptance::Native`] a
    /// picture may be a dmabuf or planar YUV and this is then `None` rather than a
    /// hidden GPU readback; those consumers read [`Self::latest`].
    pub fn latest_frame(&self) -> Option<&VideoFrame> {
        self.latest_rgba.as_ref()
    }

    /// Take whatever picture the sink is holding. Order matters: `appsink` serves
    /// both pull APIs from one queue, and asking for a regular sample pops and
    /// discards anything that is not one, including a preroll. While paused the
    /// preroll is the only picture, so it is asked for first.
    fn drain_samples(&mut self) -> bool {
        // Paused, or playing but not yet seeded: the preroll is the picture.
        let mut got = if (!self.playing || self.latest.is_none())
            && let Some(sample) = self.appsink.try_pull_preroll(gst::ClockTime::ZERO)
            && let Some(frame) = frame_from_sample(&sample, self.acceptance)
        {
            self.record_caps(&sample);
            self.store(in_stream_time(frame, &sample));
            true
        } else {
            false
        };

        // Playing: drain to the newest so the surface never lags the pipeline.
        if self.playing {
            while let Some(sample) = self.appsink.try_pull_sample(gst::ClockTime::ZERO) {
                if let Some(frame) = frame_from_sample(&sample, self.acceptance) {
                    self.record_caps(&sample);
                    self.store(in_stream_time(frame, &sample));
                    got = true;
                }
            }
        }
        got
    }

    /// Keep `frame` as the current picture, refreshing the RGBA view
    /// [`Self::latest_frame`] hands out. Built here so that accessor stays `&self`;
    /// it costs one `Arc` clone on the RGBA path and is deliberately not built for
    /// a planar or dmabuf payload, where converting behind a `Native` consumer's
    /// back would reintroduce the CPU cost this path removes.
    fn store(&mut self, frame: Frame) {
        // Saturating: a wrap would make a fresh frame look stale forever. 2^64
        // frames is 24 billion years at 24 fps.
        self.frames_stored = self.frames_stored.saturating_add(1);
        self.latest_rgba = match frame.payload {
            FramePayload::Rgba(_) => frame.to_video_frame(),
            _ => None,
        };
        self.latest = Some(frame);
    }

    fn record_caps(&self, sample: &gst::Sample) {
        if let Some(caps) = sample.caps()
            && let Ok(mut o) = self.observed.lock()
            && o.report.negotiated_caps.is_none()
        {
            o.report.negotiated_caps = Some(caps.to_string());
        }
    }

    /// When `error` came from a hardware decoder that refused the stream (a
    /// picture larger than the block takes, a profile it lacks), log why, demote
    /// that decoder for this source and reopen at the same position, so
    /// autoplugging moves on to the next one. `true` if it did.
    ///
    /// A decoder is refused once per source: if it comes back and fails again, the
    /// error stands.
    fn retry_past_refused_decoder(&mut self, error: &gst::message::Error) -> bool {
        let Some(element) = error
            .src()
            .and_then(|src| src.clone().downcast::<gst::Element>().ok())
        else {
            return false;
        };
        let name = element.name();
        if !crate::hw::looks_like_video_decoder(&name)
            || !DecoderKind::classify(&name).is_hardware()
        {
            return false;
        }
        let Some(factory) = element.factory().map(|f| f.name().to_string()) else {
            return false;
        };
        if self.refused.contains(&factory) {
            return false;
        }
        let (media_type, size) = decoder_input(&element);
        let detail = error.error().to_string();
        let refused = vec![(factory.clone(), detail.clone())];
        if let Some(media_type) = media_type {
            let reason = fallback::explain(
                Direction::Decode,
                media_type,
                false,
                std::slice::from_ref(&factory),
                &refused,
                size,
                v4l2::devices(),
            );
            fallback::report_once(Direction::Decode, media_type, &reason);
            self.refusal = Some((media_type, reason.why(Direction::Decode, media_type)));
        } else {
            tracing::warn!("media: hardware decoder {factory} refused the stream ({detail})");
        }
        if let Ok(mut o) = self.observed.lock() {
            o.refused.extend(refused);
            o.report = HwReport::default();
        }
        self.refused.insert(factory);

        let resume_at = self.last_position.unwrap_or(0);
        let _ = self.playbin.set_state(gst::State::Null);
        if let Some(copy) = &self.copy_stage {
            copy.set_property("drop-allocation", false);
        }
        apply_decoder_ranks(self.policy, &self.refused);
        self.seek_in_flight = false;
        self.seek_target_ms = None;
        self.resume = Some((resume_at, self.playing));
        let _ = self.playbin.set_state(gst::State::Paused);
        true
    }

    fn explain_missing_decoder(&mut self, missing: &gst::StructureRef) {
        let caps = missing
            .get::<gst::Caps>("detail")
            .ok()
            .and_then(|caps| caps.structure(0).map(ToOwned::to_owned));
        if missing.get::<&str>("type").ok() == Some("decoder")
            && let Some(st) = &caps
        {
            self.missing_decoder = Some(crate::event::codec_name(
                st.name().as_str(),
                st.get::<i32>("mpegversion").ok(),
            ));
        }
        let Some(media_type) = missing
            .get::<gst::Caps>("detail")
            .ok()
            .and_then(|caps| caps.structure(0).map(|st| st.name().to_string()))
            .and_then(|name| v4l2::element_media_type(&name))
        else {
            return;
        };
        let why = match &self.refusal {
            Some((mt, why)) if *mt == media_type => why.clone(),
            _ => fallback::explain(
                Direction::Decode,
                media_type,
                false,
                &hardware_decoders_for(media_type),
                &[],
                None,
                v4l2::devices(),
            )
            .why(Direction::Decode, media_type),
        };
        let text = fallback::report_unplayable(media_type, &why);
        self.refusal = Some((media_type, text));
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
                    // Clamped on the same line, so the conversion cannot fail; 0 is the
                    // honest reading of a percentage that was not one.
                    let pct = u8::try_from(b.percent().clamp(0, 100)).unwrap_or(0);
                    self.buffering = pct;
                    if self.last_buffering != Some(pct) {
                        self.last_buffering = Some(pct);
                        events.push(MediaEvent::Buffering { percent: pct });
                    }
                }
                MessageView::AsyncDone(_) => {
                    self.seek_in_flight = self.deferred_seek.is_some();
                    if let Some((ms, playing)) = self.resume.take() {
                        if ms > 0 {
                            self.seek(ms, SeekMode::Accurate);
                        }
                        if playing {
                            self.set_playing(true);
                        }
                    }
                }
                MessageView::Eos(_) => {
                    self.draining_output = true;
                }
                MessageView::Error(e) => {
                    if self.retry_past_refused_decoder(e) {
                        while self.bus.pop().is_some() {}
                        break;
                    }
                    self.playing = false;
                    let message = match &self.refusal {
                        Some((_, why)) => format!("{} ({why})", e.error()),
                        None => e.error().to_string(),
                    };
                    if let Some(codec) = self.missing_decoder.take() {
                        events.push(MediaEvent::MissingDecoder { codec });
                    }
                    events.push(MediaEvent::Error(message));
                }
                MessageView::Element(el) => {
                    if let Some(st) = el.structure()
                        && st.name() == "missing-plugin"
                    {
                        self.explain_missing_decoder(st);
                    }
                }
                MessageView::Toc(toc) => {
                    let (toc, _) = toc.toc();
                    self.chapters = chapters_from_toc(&toc);
                }
                _ => {}
            }
        }

        if !self.output_error_reported
            && let Some(error) = self
                .governed_output
                .as_ref()
                .and_then(|output| output.error())
        {
            self.output_error_reported = true;
            self.draining_output = false;
            self.playing = false;
            events.push(MediaEvent::Error(error));
        }
        if self.draining_output
            && self
                .governed_output
                .as_ref()
                .is_none_or(|output| output.drained())
        {
            self.draining_output = false;
            self.playing = false;
            events.push(MediaEvent::Ended);
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

/// Record a newly instantiated element. For the stream's video decoder: log
/// which one it is, switch the RGBA bin's copy stage on for a V4L2 decoder, and
/// for a software one say why, once.
fn on_element_added(
    element: &gst::Element,
    observed: &Arc<Mutex<Observed>>,
    copy_stage: Option<&gst::Element>,
    policy: HwPolicy,
    acceptance: FrameAcceptance,
) {
    if element
        .factory()
        .is_some_and(|f| f.name() == "uridecodebin")
    {
        steer_autoplugging(element, observed, acceptance);
        return;
    }
    let name = element.name().to_string();
    let (kind, already_refused) = {
        let Ok(mut o) = observed.lock() else {
            return;
        };
        if !o.report.note_element(&name) {
            return;
        }
        (
            o.report.decoder_kind.unwrap_or(DecoderKind::Software),
            !o.refused.is_empty(),
        )
    };
    tracing::info!(
        "media: decoder {name} ({}, {})",
        kind.as_str(),
        if kind.is_hardware() {
            "hardware"
        } else {
            "software"
        }
    );
    if kind == DecoderKind::V4l2
        && let Some(copy) = copy_stage
    {
        copy.set_property("drop-allocation", true);
    }
    // A refusal has already been explained, with more detail than is known here.
    if kind.is_hardware() || already_refused {
        return;
    }
    let factory = element.factory().map(|f| f.name().to_string());
    let Some(media_type) = factory.as_deref().and_then(v4l2::element_media_type) else {
        return;
    };
    let installed = hardware_decoders_for(media_type);
    let reason = fallback::explain(
        Direction::Decode,
        media_type,
        policy == HwPolicy::Off,
        &installed,
        &[],
        None,
        v4l2::devices(),
    );
    fallback::report_once(Direction::Decode, media_type, &reason);
}

/// Two hooks on the URI decoder's autoplugging.
///
/// `autoplug-select` skips a hardware decoder that already refused the current
/// source: ranks alone cannot do this, because `playbin` sorts its factory list
/// once per registry cookie. `autoplug-query` answers the allocation query a
/// decoder sends before its source pad is linked, advertising `GstVideoMeta`:
/// an H.264 stream in MP4 negotiates on its first caps event, before `playbin`
/// links it to the sink, and without the meta a V4L2 decoder that picked
/// `memory:DMABuf` refuses to continue and the stream never prerolls.
fn steer_autoplugging(
    uridecodebin: &gst::Element,
    observed: &Arc<Mutex<Observed>>,
    acceptance: FrameAcceptance,
) {
    let refused = observed.clone();
    uridecodebin.connect("autoplug-select", false, move |vals| {
        let factory = vals.get(3)?.get::<gst::ElementFactory>().ok()?;
        let skip = refused.lock().is_ok_and(|o| {
            o.refused
                .iter()
                .any(|(name, _)| name.as_str() == factory.name().as_str())
        });
        let ty = gst::glib::Type::from_name("GstAutoplugSelectResult")?;
        gst::glib::EnumClass::with_type(ty)?.to_value(if skip { 2 } else { 0 })
    });
    // Only a consumer that takes dmabufs negotiates them early: an RGBA sink
    // advertises system-memory caps, and answering for it here would hand a V4L2
    // decoder the meta that stops it copying out of its write-combined buffers
    // (see `build_video_sink`).
    if acceptance != FrameAcceptance::Native {
        return;
    }
    uridecodebin.connect("autoplug-query", false, |vals| {
        use gst::glib::translate::ToGlibPtr;
        let element = vals.get(2)?.get::<gst::Element>().ok()?;
        let value = vals.get(3)?;
        if !value.type_().is_a(gst::Query::static_type()) {
            return Some(false.to_value());
        }
        // Borrowed, not taken: the handler answers the query in place, and taking a
        // reference would make it read-only.
        let raw_value: *const gst::glib::gobject_ffi::GValue = value.to_glib_none().0;
        let raw = unsafe { gst::glib::gobject_ffi::g_value_get_boxed(raw_value) }
            .cast::<gst::ffi::GstQuery>();
        if raw.is_null() {
            return Some(false.to_value());
        }
        let query = unsafe { gst::QueryRef::from_ptr(raw) };
        let writable = unsafe { gst::ffi::gst_mini_object_is_writable(raw.cast()) } != 0;
        if query.type_() != gst::QueryType::Allocation
            || !crate::hw::looks_like_video_decoder(&element.name())
            || !writable
        {
            return Some(false.to_value());
        }
        let api = gst_video::VideoMeta::meta_api();
        unsafe {
            gst::ffi::gst_query_add_allocation_meta(raw, api.into_glib(), std::ptr::null());
        }
        Some(true.to_value())
    });
}

/// Flatten a container TOC without inventing labels. Nested editions and titles
/// are traversed because chapters commonly sit beneath an edition node; only
/// `Chapter` entries become seek targets.
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
        if let Some(output) = &self.governed_output {
            output.close();
        }
        let _ = self.playbin.set_state(gst::State::Null);
    }
}

/// `frame` with its timestamp as a position in the stream, the scale seeks and
/// positions are given in. A buffer's timestamp counts from wherever the
/// container's clock started, which for a stream that does not start at zero is
/// not where a seek to that position lands; its segment maps one to the other.
fn in_stream_time(mut frame: Frame, sample: &gst::Sample) -> Frame {
    let stream_ms = sample
        .buffer()
        .and_then(gst::BufferRef::pts)
        .zip(
            sample
                .segment()
                .and_then(|segment| segment.downcast_ref::<gst::ClockTime>()),
        )
        .and_then(|(pts, segment)| segment.to_stream_time(pts))
        .map(gst::ClockTime::mseconds);
    if let Some(ms) = stream_ms {
        frame.pts_ms = ms;
    }
    frame
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init() {
        gst::init().expect("GStreamer initialises in the test environment");
    }

    #[test]
    fn rgba_caps_and_seek_flags_are_requested_exactly() {
        {
            init();
            // The compatibility path must never negotiate a GPU buffer its consumer cannot
            // paint.
            let caps = sink_caps(FrameAcceptance::Rgba);
            let s = caps.to_string();
            assert!(s.contains("RGBA"), "{s}");
            assert!(
                !s.contains("DMABuf"),
                "RGBA-only consumers must not be offered dmabuf: {s}"
            );
        }
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
