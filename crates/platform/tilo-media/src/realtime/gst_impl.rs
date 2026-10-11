//! The `GStreamer` implementation of the real-time path.
//!
//! Encoder: `appsrc ! convert ! <encoder> ! <parser> ! capsfilter ! appsink`.
//! Decoder: `appsrc ! <parser> ! <decoder> ! [convert] ! appsink`.
//!
//! Both sources are live and both sinks unsynchronised, so a buffer flows the
//! moment it is pushed. Samples and bus errors arrive on one channel, which
//! lets a call wait for "this frame's output or this pipeline's failure"
//! without polling either.

use std::str::FromStr;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;

use super::{
    CodecCapabilities, DecodeOutput, DecodedPicture, DecoderConfig, EncodedUnit, EncoderConfig,
    InputFormat, InputFrame, RealtimeError, StreamColor, is_keyframe,
};
use crate::frame::FrameAcceptance;
use crate::hw::realtime::{
    CodecElement, CodecFamily, StreamCodec, realtime_candidates, realtime_decoders,
    realtime_encoders,
};

/// How long an encoder or decoder may take to produce its first output after
/// construction. Hardware initialisation (opening the render node, creating
/// the context) happens here.
#[cfg(not(any(test, feature = "test-support")))]
fn startup_timeout() -> Duration {
    Duration::from_secs(5)
}
/// How long an encoder or decoder may take to produce its first output after
/// construction: in test builds, which share a loaded runner, the shared test
/// hang bound.
#[cfg(any(test, feature = "test-support"))]
fn startup_timeout() -> Duration {
    tilo_test_env::hang_bound()
}
/// How long one frame may take once the pipeline is running. Every element is
/// configured for one-in-one-out operation; a frame that takes longer than
/// this is treated as lost rather than blocking the caller's frame loop.
const FRAME_TIMEOUT: Duration = Duration::from_millis(500);
/// How long a running decoder waits for one unit's picture. A unit that
/// yields none (its reference was lost on the network, it only refreshes
/// state) must not stall the receiver's frame loop: measured decode times are
/// 3-25 ms per frame at 720p-1080p, so a picture that has not arrived within
/// several frame intervals (with headroom for a loaded machine) is returned
/// with the next call instead.
const DECODE_WAIT: Duration = Duration::from_millis(150);

enum Event {
    Sample(gst::Sample),
    Error(String),
    /// Every buffer pushed before end-of-stream has come out.
    Eos,
}

/// Initialise `GStreamer` once for the process.
fn init() -> Result<(), RealtimeError> {
    static INIT: OnceLock<Result<(), String>> = OnceLock::new();
    INIT.get_or_init(|| gst::init().map_err(|e| e.to_string()))
        .clone()
        .map_err(RealtimeError::Pipeline)?;
    crate::extensions::refresh_registry();
    Ok(())
}

fn registry_has(name: &str) -> bool {
    gst::ElementFactory::find(name).is_some()
}

/// Rebuild the plugin registry from the files on disk now, whether or not the
/// merged extensions changed, so elements that appeared since start-up become
/// candidates. Changes to the codec pack are picked up automatically by every
/// encoder and decoder construction ([`crate::extensions`]); this is for a
/// caller that knows something else changed (a driver update).
///
/// Encoders and decoders never cache their choice across constructions, so
/// the next one built after this sees the new registry. A plugin that was
/// already loaded into this process keeps the element list it registered when
/// it loaded: new plugins (the Cisco `openh264` plugin, libav) are usable at
/// once, while new hardware entry points of an already-loaded VA-API plugin
/// (a freeworld Mesa driver adding H.264 encode) are seen by processes started
/// afterwards. Until then selection falls through to the next candidate, as
/// it does for any listed element that fails its start-up probe.
pub fn rescan_registry() {
    crate::extensions::rebuild_registry();
}

/// What this machine can encode and decode right now, read from the registry.
///
/// Hardware plugins register one element per capability the device reports
/// (the VA-API plugin lists `vaav1enc` only where the driver exposes an AV1
/// encode entry point), so presence in the registry is the capability.
pub fn local_capabilities() -> CodecCapabilities {
    let mut caps = CodecCapabilities::default();
    if init().is_err() {
        return caps;
    }
    for codec in StreamCodec::ALL {
        for e in realtime_encoders(codec) {
            if registry_has(e.element) {
                if e.is_hardware() {
                    caps.hardware_encode.insert(codec);
                } else {
                    caps.software_encode.insert(codec);
                }
            }
        }
        for e in realtime_decoders(codec) {
            if registry_has(e.element) {
                if e.is_hardware() {
                    // The first hardware decoder listed is the one a stream
                    // gets, so its limit is the codec's.
                    if caps.hardware_decode.insert(codec)
                        && let Some(limit) = decoder_size_limit(e)
                    {
                        caps.hardware_decode_max.insert(codec, limit);
                    }
                } else {
                    caps.software_decode.insert(codec);
                }
            }
        }
    }
    caps
}

/// The largest picture a hardware decoder element takes. VA-API, NVDEC and
/// stateful V4L2 elements state their device's range in their sink caps.
/// Stateless V4L2 elements (`v4l2sl*dec`, the decoders of ARM boards) state
/// none; only the kernel's frame-size enumeration knows theirs, so they are
/// taken to stop at 1920x1088, where the Hantro G1 and its peers do, rather
/// than be sent pictures they would hand back to the CPU.
fn decoder_size_limit(element: &CodecElement) -> Option<(u32, u32)> {
    const STATELESS_V4L2_MAX: (u32, u32) = (1920, 1088);
    let stated = gst::ElementFactory::find(element.element).and_then(|factory| {
        factory
            .static_pad_templates()
            .iter()
            .filter(|template| template.direction() == gst::PadDirection::Sink)
            .flat_map(|template| {
                let caps = template.caps();
                caps.iter()
                    .filter_map(|structure| {
                        let side = |field: &str| {
                            structure
                                .get::<gst::IntRange<i32>>(field)
                                .map(|range| range.max())
                                .or_else(|_| structure.get::<i32>(field))
                                .ok()
                                .and_then(|max| u32::try_from(max).ok())
                        };
                        Some((side("width")?, side("height")?))
                    })
                    .collect::<Vec<_>>()
            })
            // An open range (`[1, MAX]`) states no limit.
            .filter(|(w, h)| *w < 32_768 && *h < 32_768)
            .max_by_key(|(w, h)| u64::from(*w).saturating_mul(u64::from(*h)))
    });
    stated.or_else(|| {
        (element.family == CodecFamily::V4l2 && element.element.starts_with("v4l2sl"))
            .then_some(STATELESS_V4L2_MAX)
    })
}

fn make(factory: &str) -> Result<gst::Element, String> {
    gst::ElementFactory::make(factory)
        .build()
        .map_err(|e| format!("{factory}: {e}"))
}

/// Set a numeric property, clamped into the property's declared range and
/// converted to its declared type. A property the element does not have is
/// skipped: the tables list optional tuning, not requirements.
fn set_number(element: &gst::Element, name: &str, value: i64) {
    let Some(pspec) = element.find_property(name) else {
        return;
    };
    let ty = pspec.value_type();
    let v: gst::glib::Value = if ty == u32::static_type() {
        let p = pspec.downcast_ref::<gst::glib::ParamSpecUInt>();
        let (lo, hi) = p.map_or((0, u32::MAX), |p| (p.minimum(), p.maximum()));
        u32::try_from(value.clamp(i64::from(lo), i64::from(hi)))
            .unwrap_or(hi)
            .to_value()
    } else if ty == i32::static_type() {
        let p = pspec.downcast_ref::<gst::glib::ParamSpecInt>();
        let (lo, hi) = p.map_or((i32::MIN, i32::MAX), |p| (p.minimum(), p.maximum()));
        i32::try_from(value.clamp(i64::from(lo), i64::from(hi)))
            .unwrap_or(hi)
            .to_value()
    } else if ty == u64::static_type() {
        let p = pspec.downcast_ref::<gst::glib::ParamSpecUInt64>();
        let (lo, hi) = p.map_or((0, u64::MAX), |p| (p.minimum(), p.maximum()));
        u64::try_from(value).unwrap_or(0).clamp(lo, hi).to_value()
    } else if ty == i64::static_type() {
        let p = pspec.downcast_ref::<gst::glib::ParamSpecInt64>();
        let (lo, hi) = p.map_or((i64::MIN, i64::MAX), |p| (p.minimum(), p.maximum()));
        value.clamp(lo, hi).to_value()
    } else {
        tracing::debug!(
            "media: {name} on {} is not an integer property",
            element.name()
        );
        return;
    };
    element.set_property_from_value(name, &v);
}

/// Set an enum, boolean or string property from its string form, skipping a
/// property the element lacks or a value it does not accept.
fn set_str(element: &gst::Element, name: &str, value: &str) {
    let Some(pspec) = element.find_property(name) else {
        return;
    };
    let ty = pspec.value_type();
    if ty.is_a(gst::glib::Type::ENUM) {
        let Some(class) = gst::glib::EnumClass::with_type(ty) else {
            return;
        };
        if class.value_by_nick(value).is_none() && class.value_by_name(value).is_none() {
            tracing::debug!("media: {} does not accept {name}={value}", element.name());
            return;
        }
    }
    element.set_property_from_str(name, value);
}

/// The property carrying an element's target bitrate, and whether it counts
/// in kilobits.
fn bitrate_property(element: &str) -> Option<(&'static str, bool)> {
    match element {
        "openh264enc" | "rav1enc" => Some(("bitrate", false)),
        "vp9enc" => Some(("target-bitrate", false)),
        "av1enc" => Some(("target-bitrate", true)),
        e if e.starts_with("va") || e.starts_with("nv") => Some(("bitrate", true)),
        _ => None,
    }
}

fn apply_bitrate(element: &gst::Element, spec: &CodecElement, bits_per_second: u64) {
    let Some((prop, kilobits)) = bitrate_property(spec.element) else {
        return;
    };
    if bits_per_second == 0 {
        return;
    }
    let value = if kilobits {
        (bits_per_second / 1000).max(1)
    } else {
        bits_per_second
    };
    set_number(element, prop, i64::try_from(value).unwrap_or(i64::MAX));
}

/// Configure an encoder for one-in-one-out, low-latency operation.
fn tune_encoder(element: &gst::Element, spec: &CodecElement, config: &EncoderConfig) {
    // "Only on request" is spelled as the longest interval each encoder takes.
    let interval = if config.keyframe_interval == 0 {
        i64::MAX
    } else {
        i64::from(config.keyframe_interval)
    };
    let threads = std::thread::available_parallelism()
        .map_or(4, std::num::NonZeroUsize::get)
        .min(8);
    let threads = i64::try_from(threads).unwrap_or(4);
    match spec.family {
        CodecFamily::Va => {
            set_number(element, "b-frames", 0);
            set_number(element, "key-int-max", interval);
            set_number(element, "ref-frames", 1);
            if config.bitrate_bps > 0 {
                set_str(element, "rate-control", "cbr");
            }
            // AV1: no hierarchical GOP, so every frame is output as soon as
            // it is encoded.
            set_number(element, "hierarchical-level", 1);
            set_number(element, "gf-group-size", 1);
        }
        CodecFamily::NvCodec => {
            set_str(element, "preset", "low-latency-hq");
            set_str(element, "tune", "ultra-low-latency");
            set_str(element, "zerolatency", "true");
            set_number(element, "bframes", 0);
            set_number(element, "b-frames", 0);
            set_number(element, "gop-size", interval);
            if config.bitrate_bps > 0 {
                set_str(element, "rc-mode", "cbr");
            }
        }
        CodecFamily::V4l2 | CodecFamily::Vulkan => {}
        CodecFamily::Software => match spec.element {
            "openh264enc" => {
                set_str(
                    element,
                    "usage-type",
                    if config.screen_content {
                        "screen"
                    } else {
                        "camera"
                    },
                );
                set_str(element, "rate-control", "bitrate");
                set_str(element, "complexity", "low");
                set_str(element, "enable-frame-skip", "false");
                set_number(element, "gop-size", interval);
                set_number(element, "multi-thread", threads);
            }
            "vp9enc" => {
                set_number(element, "deadline", 1);
                set_number(element, "cpu-used", 8);
                set_number(element, "lag-in-frames", 0);
                set_str(element, "auto-alt-ref", "false");
                set_str(element, "end-usage", "cbr");
                set_number(element, "keyframe-max-dist", interval);
                set_number(element, "threads", threads);
                set_str(element, "row-mt", "true");
                set_number(element, "tile-columns", 2);
            }
            "rav1enc" => {
                set_number(element, "speed-preset", 10);
                set_str(element, "low-latency", "true");
                set_number(element, "max-key-frame-interval", interval);
                set_number(element, "threads", threads);
            }
            "av1enc" => {
                set_str(element, "usage-profile", "realtime");
                set_number(element, "cpu-used", 10);
                set_number(element, "lag-in-frames", 0);
                set_str(element, "end-usage", "cbr");
                set_number(element, "keyframe-max-dist", interval);
                set_number(element, "threads", threads);
                set_str(element, "row-mt", "true");
            }
            _ => {}
        },
    }
    apply_bitrate(element, spec, config.bitrate_bps);
}

/// Configure a decoder for immediate output.
fn tune_decoder(element: &gst::Element) {
    // dav1d: one frame of delay, every core.
    set_number(element, "max-frame-delay", 1);
    set_number(element, "n-threads", 0);
    set_number(element, "threads", 0);
    set_number(element, "max-errors", -1);
}

fn stream_color_to_gst(color: StreamColor) -> gst_video::VideoColorimetry {
    gst_video::VideoColorimetry::new(
        if color.full_range {
            gst_video::VideoColorRange::Range0_255
        } else {
            gst_video::VideoColorRange::Range16_235
        },
        gst_video::VideoColorMatrix::from_iso(u32::from(color.matrix)),
        gst_video::VideoTransferFunction::from_iso(u32::from(color.transfer)),
        gst_video::VideoColorPrimaries::from_iso(u32::from(color.primaries)),
    )
}

fn stream_color_from_info(info: &gst_video::VideoInfo) -> StreamColor {
    let c = info.colorimetry();
    let code = |v: u32, default: u8| match u8::try_from(v) {
        Ok(0) | Err(_) => default,
        Ok(n) => n,
    };
    StreamColor {
        primaries: code(c.primaries().to_iso(), 1),
        transfer: code(c.transfer().to_iso(), 1),
        // Matrix 0 is a real value (identity), so only "unknown" defaults.
        matrix: match c.matrix() {
            gst_video::VideoColorMatrix::Unknown => 1,
            m => u8::try_from(m.to_iso()).unwrap_or(1),
        },
        full_range: c.range() == gst_video::VideoColorRange::Range0_255,
    }
}

/// The raw caps the encoder's appsrc announces for system-memory input.
fn raw_input_caps(config: &EncoderConfig) -> Result<gst::Caps, String> {
    let format = gst_video::VideoFormat::from_string(config.input.gst_format());
    let mut builder = gst_video::VideoInfo::builder(format, config.width, config.height).fps(
        gst::Fraction::new(i32::try_from(config.fps.max(1)).unwrap_or(30), 1),
    );
    // The stream colour describes the *encoded* samples. RGB input keeps its
    // own (sRGB, full range) signalling and is converted into the stream's;
    // labelling RGB as limited-range BT.709 would make the converter squeeze
    // it a second time. YUV input already is the stream's samples.
    let yuv_color = stream_color_to_gst(config.color);
    if matches!(config.input, InputFormat::Nv12 | InputFormat::P010) {
        builder = builder.colorimetry(&yuv_color);
    }
    let info = builder.build().map_err(|e| format!("input caps: {e}"))?;
    let mut caps = info.to_caps().map_err(|e| format!("input caps: {e}"))?;
    if let Some(hdr) = config.hdr
        && let Some(caps) = caps.get_mut()
    {
        let coord = |(x, y): (u16, u16)| gst_video::VideoMasteringDisplayInfoCoordinate { x, y };
        let [g, b, r] = hdr.display_primaries;
        gst_video::VideoMasteringDisplayInfo::new(
            [coord(g), coord(b), coord(r)],
            coord(hdr.white_point),
            hdr.max_luminance,
            hdr.min_luminance,
        )
        .add_to_caps(caps);
        gst_video::VideoContentLightLevel::new(hdr.max_cll, hdr.max_fall).add_to_caps(caps);
    }
    Ok(caps)
}

/// A running pipeline plus the channel its samples and errors arrive on.
struct Running {
    pipeline: gst::Pipeline,
    appsrc: gst_app::AppSrc,
    events: Receiver<Event>,
}

impl Running {
    fn start(
        pipeline: gst::Pipeline,
        appsrc: gst_app::AppSrc,
        appsink: &gst_app::AppSink,
    ) -> Result<Self, String> {
        let (tx, rx) = mpsc::channel::<Event>();
        let sample_tx = Mutex::new(tx.clone());
        appsink.set_callbacks(
            gst_app::AppSinkCallbacks::builder()
                .new_sample(move |sink| {
                    let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                    if let Ok(tx) = sample_tx.lock() {
                        let _ = tx.send(Event::Sample(sample));
                    }
                    Ok(gst::FlowSuccess::Ok)
                })
                .propose_allocation(|_sink, query| {
                    query.add_allocation_meta::<gst_video::VideoMeta>(None);
                    true
                })
                .build(),
        );
        let bus = pipeline.bus().ok_or("pipeline has no bus")?;
        let bus_tx = Mutex::new(tx);
        bus.set_sync_handler(move |_bus, msg| {
            let event = match msg.view() {
                gst::MessageView::Error(err) => {
                    let src = msg
                        .src()
                        .map_or_else(|| "pipeline".to_string(), |s| s.name().to_string());
                    Some(Event::Error(format!("{src}: {}", err.error())))
                }
                gst::MessageView::Eos(_) => Some(Event::Eos),
                _ => None,
            };
            if let Some(event) = event
                && let Ok(tx) = bus_tx.lock()
            {
                let _ = tx.send(event);
            }
            gst::BusSyncReply::Drop
        });
        pipeline
            .set_state(gst::State::Playing)
            .map_err(|e| format!("could not start: {e}"))?;
        Ok(Self {
            pipeline,
            appsrc,
            events: rx,
        })
    }

    /// Wait up to `timeout` for the next sample; an error on the bus fails
    /// the wait at once.
    fn next_sample(&self, timeout: Duration) -> Result<Option<gst::Sample>, String> {
        match self.events.recv_timeout(timeout) {
            Ok(Event::Sample(s)) => Ok(Some(s)),
            Ok(Event::Error(e)) => Err(e),
            Ok(Event::Eos) => Err("pipeline reached end of stream".into()),
            Err(RecvTimeoutError::Timeout) => Ok(None),
            Err(RecvTimeoutError::Disconnected) => Err("pipeline stopped".into()),
        }
    }

    /// Every sample already delivered, without waiting.
    fn drain(&self) -> Result<Vec<gst::Sample>, String> {
        let mut out = Vec::new();
        while let Ok(ev) = self.events.try_recv() {
            match ev {
                Event::Sample(s) => out.push(s),
                Event::Error(e) => return Err(e),
                Event::Eos => return Err("pipeline reached end of stream".into()),
            }
        }
        Ok(out)
    }

    fn push(&self, buffer: gst::Buffer) -> Result<(), String> {
        self.appsrc
            .push_buffer(buffer)
            .map(|_| ())
            .map_err(|e| format!("push: {e:?}"))
    }
}

impl Drop for Running {
    /// Shut the pipeline down on its own thread. Stopping flushes each
    /// element, and an element blocked inside its codec library holds its
    /// stream lock until the library returns; the caller (a compositor frame
    /// loop, a call's media thread) must not wait on that.
    fn drop(&mut self) {
        let pipeline = self.pipeline.clone();
        let spawned = std::thread::Builder::new()
            .name("media-rt-teardown".into())
            .spawn(move || {
                let _ = pipeline.set_state(gst::State::Null);
            });
        if spawned.is_err() {
            let _ = self.pipeline.set_state(gst::State::Null);
        }
    }
}

// ---------------------------------------------------------------------------
// Encoder
// ---------------------------------------------------------------------------

/// A low-latency encoder: one frame in, its compressed bytes out.
///
/// See the [module docs](super) for how the element is chosen.
pub struct RealtimeEncoder {
    config: EncoderConfig,
    spec: &'static CodecElement,
    running: Running,
    encoder: gst::Element,
    /// The bitstream parser after the encoder, whose caps carry the coded
    /// size.
    parser: gst::Element,
    /// The element in front of the encoder (`vapostproc` on the dmabuf path),
    /// asked whether it can import a given dmabuf layout.
    convert: gst::Element,
    /// Whether a dmabuf import path is wired up (VA-API with `vapostproc`).
    dmabuf_path: bool,
    /// The caps currently set on the appsrc, so switching between system
    /// memory and dmabuf input renegotiates only when the kind changes.
    current_caps: gst::Caps,
    raw_caps: gst::Caps,
    force_next_keyframe: bool,
    first_output_seen: bool,
    #[cfg(unix)]
    dmabuf_allocator: Option<DmabufAllocator>,
}

impl std::fmt::Debug for RealtimeEncoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RealtimeEncoder")
            .field("element", &self.spec.element)
            .field("codec", &self.config.codec)
            .field("width", &self.config.width)
            .field("height", &self.config.height)
            .finish_non_exhaustive()
    }
}

impl RealtimeEncoder {
    /// Build an encoder for `config`, trying each installed candidate in
    /// policy order until one encodes a probe frame.
    ///
    /// # Errors
    ///
    /// [`RealtimeError::InvalidConfig`] for dimensions no candidate can take,
    /// [`RealtimeError::Unavailable`] when no candidate is installed or every
    /// installed one failed its probe.
    pub fn new(config: &EncoderConfig) -> Result<Self, RealtimeError> {
        config.validate()?;
        init()?;
        let candidates =
            realtime_candidates(realtime_encoders(config.codec), config.policy, registry_has);
        let mut tried = Vec::new();
        for spec in candidates {
            match Self::try_build(config, spec) {
                Ok(enc) => {
                    tracing::info!(
                        codec = config.codec.wire_label(),
                        element = %spec.describe(),
                        dmabuf = enc.dmabuf_path,
                        passed_over = ?tried,
                        "media: real-time encoder selected"
                    );
                    if !spec.is_hardware() {
                        crate::hw::fallback::report_realtime(
                            crate::hw::fallback::Direction::Encode,
                            config.codec,
                            config.policy,
                            realtime_encoders(config.codec),
                            registry_has,
                            &tried,
                            Some((config.width, config.height)),
                        );
                    }
                    return Ok(enc);
                }
                Err(e) => {
                    tracing::debug!("media: encoder {} failed its probe: {e}", spec.element);
                    tried.push(format!("{}: {e}", spec.element));
                }
            }
        }
        Err(RealtimeError::Unavailable {
            codec: config.codec,
            tried,
        })
    }

    fn try_build(config: &EncoderConfig, spec: &'static CodecElement) -> Result<Self, String> {
        let pipeline = gst::Pipeline::new();
        let raw_caps = raw_input_caps(config)?;
        let appsrc = gst_app::AppSrc::builder()
            .caps(&raw_caps)
            .is_live(true)
            .format(gst::Format::Time)
            .do_timestamp(false)
            .stream_type(gst_app::AppStreamType::Stream)
            .build();
        // VA-API with dmabuf input: `vapostproc` imports the compositor's
        // dmabuf and converts it to NV12 on the GPU, so the frame never
        // reaches system memory. Everything else goes through `videoconvert`,
        // including system-memory frames for VA-API encoders (which upload
        // the NV12 themselves). Measured on radeonsi at 720p, `vapostproc`'s
        // RGB-to-YUV conversion lands at 39 dB PSNR against 50 dB for
        // `videoconvert`, and its RGBA path swaps channels, so it is used
        // only where it saves the copy.
        let use_vapostproc = config.dmabuf_input
            && spec.family == CodecFamily::Va
            && !matches!(config.input, InputFormat::Rgba)
            && registry_has("vapostproc");
        let convert = if use_vapostproc {
            make("vapostproc")?
        } else {
            let c = make("videoconvert")?;
            set_number(&c, "n-threads", 0);
            c
        };
        // Pin the colour of the samples the encoder receives, so the
        // conversion targets the stream's colour and the encoder signals it.
        // `vapostproc` is left to report its own.
        // A smaller output is scaled before encoding: `vapostproc` scales as
        // it converts; otherwise `videoscale` follows the conversion.
        let scale_to = config
            .output_size
            .filter(|size| *size != (config.width, config.height));
        let scaler = match scale_to {
            Some(_) if !use_vapostproc => Some(make("videoscale")?),
            _ => None,
        };
        let mut target = if use_vapostproc {
            gst::Caps::builder("video/x-raw")
        } else {
            gst::Caps::builder("video/x-raw")
                .field("colorimetry", stream_color_to_gst(config.color).to_string())
        };
        if let Some((width, height)) = scale_to {
            target = target
                .field("width", i32::try_from(width).unwrap_or(i32::MAX))
                .field("height", i32::try_from(height).unwrap_or(i32::MAX));
        }
        let target = if use_vapostproc && scale_to.is_none() {
            gst::Caps::new_any()
        } else {
            target.build()
        };
        let yuv_filter = gst::ElementFactory::make("capsfilter")
            .property("caps", target)
            .build()
            .map_err(|e| format!("capsfilter: {e}"))?;
        let convert_handle = convert.clone();
        let encoder = make(spec.element)?;
        tune_encoder(&encoder, spec, config);
        let parser = make(config.codec.parser())?;
        let parser_handle = parser.clone();
        // Repeat parameter sets with every keyframe so a receiver can join at
        // any of them.
        set_number(&parser, "config-interval", -1);
        let out_caps = gst::Caps::from_str(config.codec.stream_caps())
            .map_err(|e| format!("stream caps: {e}"))?;
        let filter = gst::ElementFactory::make("capsfilter")
            .property("caps", &out_caps)
            .build()
            .map_err(|e| format!("capsfilter: {e}"))?;
        let appsink = gst_app::AppSink::builder()
            .sync(false)
            .max_buffers(0)
            .drop(false)
            .enable_last_sample(false)
            .build();
        let src_el = appsrc.clone().upcast::<gst::Element>();
        let sink_el = appsink.clone().upcast::<gst::Element>();
        let mut chain = vec![&src_el, &convert];
        if let Some(scaler) = scaler.as_ref() {
            chain.push(scaler);
        }
        chain.extend([&yuv_filter, &encoder, &parser, &filter, &sink_el]);
        pipeline
            .add_many(chain.iter().copied())
            .map_err(|e| format!("assemble: {e}"))?;
        gst::Element::link_many(chain.iter().copied()).map_err(|e| format!("link: {e}"))?;

        let running = Running::start(pipeline, appsrc, &appsink)?;
        let mut enc = Self {
            config: config.clone(),
            spec,
            running,
            encoder,
            convert: convert_handle,
            parser: parser_handle,
            dmabuf_path: use_vapostproc,
            current_caps: raw_caps.clone(),
            raw_caps,
            force_next_keyframe: false,
            first_output_seen: false,
            #[cfg(unix)]
            dmabuf_allocator: None,
        };
        enc.probe()?;
        Ok(enc)
    }

    /// Encode one mid-grey frame and require output. This is what turns "the
    /// element is registered" into "the element works on this machine at
    /// this size": a hardware encoder that is listed but cannot open its
    /// device, or cannot take these dimensions, fails here.
    fn probe(&mut self) -> Result<(), String> {
        let len = self
            .config
            .input
            .frame_len(self.config.width, self.config.height)
            .ok_or("frame size overflows")?;
        let fill = match self.config.input {
            InputFormat::Nv12 | InputFormat::P010 => 0x80,
            InputFormat::Rgba | InputFormat::Bgra | InputFormat::Bgrx => 0x60,
        };
        let buffer = gst_buffer(vec![fill; len], Duration::ZERO);
        self.running.push(buffer)?;
        match self.running.next_sample(startup_timeout())? {
            Some(_) => {}
            None => return Err(format!("no output within {:?}", startup_timeout())),
        }
        // Discard any trailing output of the probe, and make the first real
        // frame a keyframe: the probe consumed the stream's first one.
        let _ = self.running.drain()?;
        self.force_next_keyframe = true;
        Ok(())
    }

    /// The element doing the work.
    pub fn element(&self) -> &'static CodecElement {
        self.spec
    }

    /// The configuration the encoder was built with (with the current
    /// bitrate).
    pub fn config(&self) -> &EncoderConfig {
        &self.config
    }

    /// The frame size the encoder actually codes, read from the bitstream
    /// parser once output has flowed. It can exceed the configured size when
    /// an encoder pads to its alignment without signalling the display size
    /// (see [`DecoderConfig::display_size`]); a sender whose transport
    /// carries the size should announce the configured one.
    pub fn coded_size(&self) -> Option<(u32, u32)> {
        let caps = self.parser.static_pad("src")?.current_caps()?;
        let s = caps.structure(0)?;
        let w = s.get::<i32>("width").ok()?;
        let h = s.get::<i32>("height").ok()?;
        Some((u32::try_from(w).ok()?, u32::try_from(h).ok()?))
    }

    /// Whether [`InputFrame::Dmabuf`] is imported without a CPU copy.
    pub fn accepts_dmabuf(&self) -> bool {
        self.dmabuf_path
    }

    /// Whether a dmabuf of this layout can be imported without a copy. A
    /// driver lists the DRM format/modifier pairs its video post-processor
    /// can read (radeonsi, for one, lists only its own tiled layouts and not
    /// linear); a caller holding a buffer this rejects reads it back and
    /// passes [`InputFrame::Packed`] instead.
    #[cfg(unix)]
    pub fn accepts_dmabuf_format(&self, fourcc: u32, modifier: u64) -> bool {
        if !self.dmabuf_path {
            return false;
        }
        let Ok(caps) = self.dmabuf_caps(fourcc, modifier) else {
            return false;
        };
        self.convert
            .static_pad("sink")
            .is_some_and(|pad| pad.query_accept_caps(&caps))
    }

    #[cfg(unix)]
    fn dmabuf_caps(&self, fourcc: u32, modifier: u64) -> Result<gst::Caps, RealtimeError> {
        let info = gst_video::VideoInfo::builder(
            gst_video::VideoFormat::DmaDrm,
            self.config.width,
            self.config.height,
        )
        .fps(gst::Fraction::new(
            i32::try_from(self.config.fps.max(1)).unwrap_or(30),
            1,
        ))
        .build()
        .map_err(|e| RealtimeError::InvalidInput(format!("dmabuf info: {e}")))?;
        gst_video::VideoInfoDmaDrm::new(info, fourcc, modifier)
            .to_caps()
            .map_err(|e| RealtimeError::InvalidInput(format!("dmabuf caps: {e}")))
    }

    /// Make the next frame a keyframe (with parameter sets), for a receiver
    /// that just joined or lost packets.
    pub fn force_keyframe(&mut self) {
        self.force_next_keyframe = true;
    }

    /// Change the target bitrate while running.
    pub fn set_bitrate(&mut self, bits_per_second: u64) {
        self.config.bitrate_bps = bits_per_second;
        apply_bitrate(&self.encoder, self.spec, bits_per_second);
    }

    /// Encode one frame captured at `pts` and return its output.
    ///
    /// Every element is configured to emit a frame's output before it needs
    /// the next one, so this normally returns exactly one unit. It waits at
    /// most a bounded time; an encoder that takes longer returns nothing now
    /// and the late output with the next call.
    ///
    /// # Errors
    ///
    /// [`RealtimeError::InvalidInput`] when the frame does not match the
    /// configuration or a dmabuf is offered without a dmabuf path, and
    /// [`RealtimeError::Pipeline`] when the encoder fails. After a pipeline
    /// error the encoder should be rebuilt.
    pub fn encode(
        &mut self,
        frame: InputFrame<'_>,
        pts: Duration,
    ) -> Result<Vec<EncodedUnit>, RealtimeError> {
        let buffer = match frame {
            InputFrame::Packed(bytes) => {
                let want = self
                    .config
                    .input
                    .frame_len(self.config.width, self.config.height)
                    .unwrap_or(usize::MAX);
                if bytes.len() != want {
                    return Err(RealtimeError::InvalidInput(format!(
                        "{} bytes for a {}x{} {:?} frame; expected {want}",
                        bytes.len(),
                        self.config.width,
                        self.config.height,
                        self.config.input
                    )));
                }
                self.set_input_caps(&self.raw_caps.clone());
                gst_buffer(bytes.to_vec(), pts)
            }
            #[cfg(unix)]
            InputFrame::Dmabuf(d) => {
                if !self.dmabuf_path {
                    return Err(RealtimeError::InvalidInput(
                        "this encoder has no dmabuf import path".into(),
                    ));
                }
                let (buffer, caps) = self.dmabuf_buffer(&d, pts)?;
                self.set_input_caps(&caps);
                buffer
            }
        };
        if self.force_next_keyframe {
            self.force_next_keyframe = false;
            let event = gst_video::DownstreamForceKeyUnitEvent::builder()
                .all_headers(true)
                .build();
            if !self.running.appsrc.send_event(event) {
                tracing::debug!("media: force-key-unit was not accepted");
            }
        }
        self.running.push(buffer).map_err(RealtimeError::Pipeline)?;
        let timeout = if self.first_output_seen {
            FRAME_TIMEOUT
        } else {
            startup_timeout()
        };
        let mut samples = Vec::new();
        if let Some(s) = self
            .running
            .next_sample(timeout)
            .map_err(RealtimeError::Pipeline)?
        {
            samples.push(s);
        }
        samples.extend(self.running.drain().map_err(RealtimeError::Pipeline)?);
        if !samples.is_empty() {
            self.first_output_seen = true;
        }
        Ok(samples
            .iter()
            .filter_map(|s| self.unit_from_sample(s))
            .collect())
    }

    /// End the stream and return every unit still in flight, in order.
    ///
    /// [`Self::encode`] hands late output back with the next frame; after
    /// the last frame there is no next call, so a recording that stops
    /// collects the rest here.
    ///
    /// # Errors
    ///
    /// [`RealtimeError::Pipeline`] when the encoder fails or does not drain.
    pub fn finish(self) -> Result<Vec<EncodedUnit>, RealtimeError> {
        self.running
            .appsrc
            .end_of_stream()
            .map_err(|e| RealtimeError::Pipeline(format!("end of stream: {e:?}")))?;
        let mut samples = Vec::new();
        loop {
            match self.running.events.recv_timeout(startup_timeout()) {
                Ok(Event::Sample(s)) => samples.push(s),
                Ok(Event::Eos) => break,
                Ok(Event::Error(e)) => return Err(RealtimeError::Pipeline(e)),
                Err(RecvTimeoutError::Timeout) => {
                    return Err(RealtimeError::Pipeline(format!(
                        "the encoder did not drain within {:?}",
                        startup_timeout()
                    )));
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(RealtimeError::Pipeline("pipeline stopped".into()));
                }
            }
        }
        Ok(samples
            .iter()
            .filter_map(|s| self.unit_from_sample(s))
            .collect())
    }

    fn unit_from_sample(&self, sample: &gst::Sample) -> Option<EncodedUnit> {
        let buffer = sample.buffer()?;
        let map = buffer.map_readable().ok()?;
        let data = map.as_slice().to_vec();
        let keyframe = !buffer.flags().contains(gst::BufferFlags::DELTA_UNIT)
            || is_keyframe(self.config.codec, &data);
        let pts = buffer
            .pts()
            .map_or(Duration::ZERO, |t| Duration::from_nanos(t.nseconds()));
        Some(EncodedUnit {
            data,
            keyframe,
            pts,
        })
    }

    fn set_input_caps(&mut self, caps: &gst::Caps) {
        if self.current_caps != *caps {
            self.running.appsrc.set_caps(Some(caps));
            self.current_caps = caps.clone();
        }
    }

    #[cfg(unix)]
    fn dmabuf_buffer(
        &mut self,
        d: &super::DmabufInput<'_>,
        pts: Duration,
    ) -> Result<(gst::Buffer, gst::Caps), RealtimeError> {
        if d.width != self.config.width || d.height != self.config.height {
            return Err(RealtimeError::InvalidInput(format!(
                "dmabuf is {}x{}, encoder is {}x{}",
                d.width, d.height, self.config.width, self.config.height
            )));
        }
        let format = gst_video::dma_drm_fourcc_to_format(d.fourcc).map_err(|_| {
            RealtimeError::InvalidInput(format!(
                "fourcc {} has no GStreamer format",
                crate::dmabuf::fourcc_str(d.fourcc)
            ))
        })?;
        if !self.accepts_dmabuf_format(d.fourcc, d.modifier) {
            return Err(RealtimeError::InvalidInput(format!(
                "{}:{:#x} is not a layout {} can import",
                crate::dmabuf::fourcc_str(d.fourcc),
                d.modifier,
                self.convert.name()
            )));
        }
        let caps = self.dmabuf_caps(d.fourcc, d.modifier)?;
        if self.dmabuf_allocator.is_none() {
            self.dmabuf_allocator = Some(DmabufAllocator::new().ok_or_else(|| {
                RealtimeError::Pipeline("no dmabuf allocator in libgstallocators".into())
            })?);
        }
        let allocator = self
            .dmabuf_allocator
            .as_ref()
            .ok_or_else(|| RealtimeError::Pipeline("no dmabuf allocator".into()))?;
        let buffer = allocator
            .wrap(d, format, pts)
            .map_err(RealtimeError::InvalidInput)?;
        Ok((buffer, caps))
    }
}

fn gst_buffer(bytes: Vec<u8>, pts: Duration) -> gst::Buffer {
    let mut buffer = gst::Buffer::from_mut_slice(bytes);
    if let Some(b) = buffer.get_mut() {
        b.set_pts(gst::ClockTime::from_nseconds(
            u64::try_from(pts.as_nanos()).unwrap_or(u64::MAX),
        ));
    }
    buffer
}

// ---------------------------------------------------------------------------
// Dmabuf import
// ---------------------------------------------------------------------------

/// Upload a packed BGRx/BGRA frame into a GPU-allocated dmabuf through
/// VA-API, in the driver's preferred tiled layout: the kind of buffer a
/// compositor renders into. For tests and tools that exercise the zero-copy
/// encode path without a compositor.
///
/// # Errors
///
/// [`RealtimeError::Unavailable`] without a VA-API post-processor,
/// [`RealtimeError::InvalidInput`] for another layout or a wrong length, and
/// [`RealtimeError::Pipeline`] if the upload does not produce a dmabuf.
#[cfg(unix)]
pub fn upload_to_dmabuf(
    format: InputFormat,
    bytes: &[u8],
    width: u32,
    height: u32,
) -> Result<crate::dmabuf::DmabufImage, RealtimeError> {
    init()?;
    if !matches!(format, InputFormat::Bgrx | InputFormat::Bgra) {
        return Err(RealtimeError::InvalidInput(format!(
            "{format:?} upload is not supported"
        )));
    }
    if format.frame_len(width, height) != Some(bytes.len()) {
        return Err(RealtimeError::InvalidInput(
            "frame length does not match".into(),
        ));
    }
    if !registry_has("vapostproc") {
        return Err(RealtimeError::Unavailable {
            codec: StreamCodec::H264,
            tried: vec!["vapostproc: not installed".into()],
        });
    }
    let pipeline = gst::Pipeline::new();
    let raw = gst_video::VideoInfo::builder(
        gst_video::VideoFormat::from_string(format.gst_format()),
        width,
        height,
    )
    .fps(gst::Fraction::new(30, 1))
    .build()
    .and_then(|i| i.to_caps())
    .map_err(|e| RealtimeError::InvalidInput(e.to_string()))?;
    let appsrc = gst_app::AppSrc::builder()
        .caps(&raw)
        .format(gst::Format::Time)
        .build();
    let vpp = make("vapostproc").map_err(RealtimeError::Pipeline)?;
    let appsink = gst_app::AppSink::builder()
        .caps(
            &gst::Caps::builder("video/x-raw")
                .features(["memory:DMABuf"])
                .field("format", "DMA_DRM")
                .build(),
        )
        .sync(false)
        .build();
    let chain = [
        appsrc.upcast_ref::<gst::Element>(),
        &vpp,
        appsink.upcast_ref::<gst::Element>(),
    ];
    pipeline
        .add_many(chain)
        .and_then(|()| gst::Element::link_many(chain))
        .map_err(|e| RealtimeError::Pipeline(e.to_string()))?;
    appsink.set_callbacks(
        gst_app::AppSinkCallbacks::builder()
            .propose_allocation(|_sink, query| {
                query.add_allocation_meta::<gst_video::VideoMeta>(None);
                true
            })
            .build(),
    );
    pipeline
        .set_state(gst::State::Playing)
        .map_err(|e| RealtimeError::Pipeline(e.to_string()))?;
    let result = appsrc
        .push_buffer(gst_buffer(bytes.to_vec(), Duration::ZERO))
        .map_err(|e| RealtimeError::Pipeline(format!("{e:?}")))
        .and_then(|_| {
            appsink
                .try_pull_sample(gst::ClockTime::from_seconds(5))
                .ok_or_else(|| RealtimeError::Pipeline("no dmabuf came out of vapostproc".into()))
        })
        .and_then(|sample| {
            let caps = sample
                .caps()
                .ok_or_else(|| RealtimeError::Pipeline("sample without caps".into()))?;
            crate::gst_frames::dmabuf_from_sample(&sample, caps)
                .ok_or_else(|| RealtimeError::Pipeline(format!("not a dmabuf: {caps}")))
        });
    let _ = pipeline.set_state(gst::State::Null);
    result
}

#[cfg(unix)]
#[link(name = "gstallocators-1.0")]
unsafe extern "C" {
    fn gst_dmabuf_allocator_new() -> *mut gst::ffi::GstAllocator;
    /// Wraps `fd` (taking ownership) as a memory of `size` bytes.
    fn gst_dmabuf_allocator_alloc(
        allocator: *mut gst::ffi::GstAllocator,
        fd: i32,
        size: usize,
    ) -> *mut gst::ffi::GstMemory;
}

/// `libgstallocators`' dmabuf allocator, bound directly for the same reason
/// `gst_backend` binds its two read-side calls: three C symbols do not
/// justify another crate in the lock.
#[cfg(unix)]
struct DmabufAllocator(gst::Allocator);

#[cfg(unix)]
impl DmabufAllocator {
    fn new() -> Option<Self> {
        use gst::glib::translate::FromGlibPtrFull;
        // Safety: returns a new reference to a GstAllocator (or NULL).
        let raw = unsafe { gst_dmabuf_allocator_new() };
        if raw.is_null() {
            return None;
        }
        // Safety: `raw` is a non-null, owned GstAllocator reference.
        Some(Self(unsafe { gst::Allocator::from_glib_full(raw) }))
    }

    /// Wrap the planes of `d` as a buffer carrying a `GstVideoMeta`.
    fn wrap(
        &self,
        d: &super::DmabufInput<'_>,
        format: gst_video::VideoFormat,
        pts: Duration,
    ) -> Result<gst::Buffer, String> {
        use gst::glib::translate::ToGlibPtr;
        use std::os::fd::{AsRawFd, IntoRawFd};

        let mut buffer = gst::Buffer::new();
        let buf = buffer.get_mut().ok_or("new buffer is not writable")?;
        // One memory per distinct fd; planes sharing an fd share a memory.
        let mut memories: Vec<(i32, usize)> = Vec::new(); // (source fd, memory start offset)
        let mut offsets = Vec::with_capacity(d.planes.len());
        let mut strides = Vec::with_capacity(d.planes.len());
        let mut total: usize = 0;
        for plane in d.planes {
            let raw = plane.fd.as_raw_fd();
            let start = if let Some(&(_, start)) = memories.iter().find(|(fd, _)| *fd == raw) {
                start
            } else {
                let owned = plane
                    .fd
                    .try_clone_to_owned()
                    .map_err(|e| format!("dup dmabuf fd: {e}"))?;
                let size = crate::dmabuf::dmabuf_size(&owned)
                    .and_then(|n| usize::try_from(n).ok())
                    .filter(|n| *n > 0)
                    .ok_or("cannot size the dmabuf")?;
                let alloc_ptr: *mut gst::ffi::GstAllocator = self.0.to_glib_none().0;
                // Safety: `alloc_ptr` is a live dmabuf allocator; the fd is a
                // fresh duplicate whose ownership passes to the memory.
                let mem_ptr =
                    unsafe { gst_dmabuf_allocator_alloc(alloc_ptr, owned.into_raw_fd(), size) };
                if mem_ptr.is_null() {
                    return Err("dmabuf allocator refused the fd".into());
                }
                // Safety: non-null, owned GstMemory reference.
                let mem = unsafe { gst::Memory::from_glib_full(mem_ptr) };
                buf.append_memory(mem);
                let start = total;
                total = total.checked_add(size).ok_or("dmabuf sizes overflow")?;
                memories.push((raw, start));
                start
            };
            offsets.push(
                start
                    .checked_add(usize::try_from(plane.offset).map_err(|e| e.to_string())?)
                    .ok_or("plane offset overflows")?,
            );
            strides.push(i32::try_from(plane.stride).map_err(|e| e.to_string())?);
        }
        buf.set_pts(gst::ClockTime::from_nseconds(
            u64::try_from(pts.as_nanos()).unwrap_or(u64::MAX),
        ));
        gst_video::VideoMeta::add_full(
            buf,
            gst_video::VideoFrameFlags::empty(),
            format,
            d.width,
            d.height,
            &offsets,
            &strides,
        )
        .map_err(|e| format!("video meta: {e}"))?;
        Ok(buffer)
    }
}

// ---------------------------------------------------------------------------
// Decoder
// ---------------------------------------------------------------------------

/// A low-latency decoder: one access unit in, its picture out.
pub struct RealtimeDecoder {
    config: DecoderConfig,
    /// Candidates still to try, best first; the head is the running one.
    candidates: Vec<&'static CodecElement>,
    running: Option<(Running, &'static CodecElement)>,
    /// The decoder element, whose output caps carry the stream's colour
    /// before any conversion to RGB.
    decoder_element: Option<gst::Element>,
    /// The colour the sender announced out of band; see
    /// [`Self::set_stream_color`]. Shared with the decoder's pad probe.
    stream_color: std::sync::Arc<Mutex<Option<StreamColor>>>,
    /// Nothing is decodable until a keyframe arrives, after start and after
    /// every fall-through to another element.
    awaiting_keyframe: bool,
    first_output_seen: bool,
    tried: Vec<String>,
    pts: Duration,
}

impl std::fmt::Debug for RealtimeDecoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RealtimeDecoder")
            .field("codec", &self.config.codec)
            .field("element", &self.element().map(|e| e.element))
            .finish_non_exhaustive()
    }
}

impl RealtimeDecoder {
    /// Build a decoder for `config`, starting the best installed candidate.
    ///
    /// # Errors
    ///
    /// [`RealtimeError::Unavailable`] when no candidate is installed or none
    /// will start.
    pub fn new(config: DecoderConfig) -> Result<Self, RealtimeError> {
        init()?;
        let candidates =
            realtime_candidates(realtime_decoders(config.codec), config.policy, registry_has);
        let mut dec = Self {
            config,
            candidates,
            running: None,
            decoder_element: None,
            stream_color: std::sync::Arc::new(Mutex::new(None)),
            awaiting_keyframe: true,
            first_output_seen: false,
            tried: Vec::new(),
            pts: Duration::ZERO,
        };
        dec.start_next()?;
        Ok(dec)
    }

    /// Set the size the sender announced; see [`DecoderConfig::display_size`].
    pub fn set_display_size(&mut self, size: Option<(u32, u32)>) {
        self.config.display_size = size;
    }

    /// The colour the sender announced for the stream, when its transport
    /// carries one. It replaces what the decoder reports, both for the
    /// conversion to RGB and in [`DecodedPicture::color`]: measured,
    /// `GStreamer`'s VA-API encoders write no colour description, so a
    /// decoder falls back to a size-based guess (BT.601 below HD) that
    /// shifts colours, and a PQ stream decodes as SDR.
    pub fn set_stream_color(&mut self, color: Option<StreamColor>) {
        if let Ok(mut slot) = self.stream_color.lock() {
            *slot = color;
        }
    }

    /// The element currently decoding.
    pub fn element(&self) -> Option<&'static CodecElement> {
        self.running.as_ref().map(|(_, e)| *e)
    }

    /// Start the next candidate that will build and reach `PLAYING`.
    fn start_next(&mut self) -> Result<(), RealtimeError> {
        self.running = None;
        self.awaiting_keyframe = true;
        self.first_output_seen = false;
        while !self.candidates.is_empty() {
            let spec = self.candidates.remove(0);
            match self.build(spec) {
                Ok((r, element)) => {
                    tracing::info!(
                        codec = self.config.codec.wire_label(),
                        element = %spec.describe(),
                        "media: real-time decoder selected"
                    );
                    if !spec.is_hardware() {
                        crate::hw::fallback::report_realtime(
                            crate::hw::fallback::Direction::Decode,
                            self.config.codec,
                            self.config.policy,
                            realtime_decoders(self.config.codec),
                            registry_has,
                            &self.tried,
                            None,
                        );
                    }
                    self.running = Some((r, spec));
                    self.decoder_element = Some(element);
                    return Ok(());
                }
                Err(e) => self.tried.push(format!("{}: {e}", spec.element)),
            }
        }
        Err(RealtimeError::Unavailable {
            codec: self.config.codec,
            tried: self.tried.clone(),
        })
    }

    fn build(&self, spec: &'static CodecElement) -> Result<(Running, gst::Element), String> {
        let pipeline = gst::Pipeline::new();
        let in_caps = gst::Caps::from_str(self.config.codec.stream_caps())
            .map_err(|e| format!("stream caps: {e}"))?;
        let appsrc = gst_app::AppSrc::builder()
            .caps(&in_caps)
            .is_live(true)
            .format(gst::Format::Time)
            .do_timestamp(false)
            .stream_type(gst_app::AppStreamType::Stream)
            .build();
        let parser = make(self.config.codec.parser())?;
        let decoder = make(spec.element)?;
        tune_decoder(&decoder);
        let decoder_handle = decoder.clone();
        // Rewrite the decoder's output colorimetry to the announced one, so
        // the conversion downstream uses the sender's colour.
        if let Some(pad) = decoder.static_pad("src") {
            let hint = std::sync::Arc::clone(&self.stream_color);
            pad.add_probe(gst::PadProbeType::EVENT_DOWNSTREAM, move |_pad, info| {
                let announced = hint.lock().ok().and_then(|slot| *slot);
                let (Some(color), Some(gst::PadProbeData::Event(event))) =
                    (announced, info.data.as_ref())
                else {
                    return gst::PadProbeReturn::Ok;
                };
                if let gst::EventView::Caps(caps_event) = event.view() {
                    // The event shares its caps; `make_mut` copies them.
                    let mut caps = caps_event.caps_owned();
                    caps.make_mut()
                        .set("colorimetry", stream_color_to_gst(color).to_string());
                    info.data = Some(gst::PadProbeData::Event(gst::event::Caps::new(&caps)));
                }
                gst::PadProbeReturn::Ok
            });
        }
        let appsink = gst_app::AppSink::builder()
            .sync(false)
            .max_buffers(0)
            .drop(false)
            .enable_last_sample(false)
            .build();
        let mut chain: Vec<gst::Element> =
            vec![appsrc.clone().upcast::<gst::Element>(), parser, decoder];
        // Conversion to RGB runs in `videoconvert`, fed by the decoder's own
        // download to system memory. `vapostproc` is not used here: measured
        // on radeonsi at 720p it took about 23 ms per frame against 4-8 ms,
        // and its YUV-to-RGB matrix disagreed with the stream's (29 dB PSNR
        // against 50 dB for the same stream).
        let out_caps = match self.config.output {
            DecodeOutput::Rgba => Some("video/x-raw, format=(string)RGBA"),
            DecodeOutput::P010 => Some("video/x-raw, format=(string)P010_10LE"),
            DecodeOutput::Native if spec.is_hardware() => None,
            DecodeOutput::Native => {
                Some("video/x-raw, format=(string){ NV12, P010_10LE, RGBA, BGRA, BGRx, RGBx }")
            }
        };
        if let Some(caps) = out_caps {
            let convert = make("videoconvert")?;
            set_number(&convert, "n-threads", 0);
            chain.push(convert);
            let caps = gst::Caps::from_str(caps).map_err(|e| format!("output caps: {e}"))?;
            appsink.set_caps(Some(&caps));
        } else {
            let caps = gst::Caps::from_str(
                "video/x-raw(memory:DMABuf); video/x-raw, format=(string){ NV12, P010_10LE, RGBA, BGRA, BGRx, RGBx }",
            )
            .map_err(|e| format!("output caps: {e}"))?;
            appsink.set_caps(Some(&caps));
        }
        chain.push(appsink.clone().upcast::<gst::Element>());
        pipeline
            .add_many(&chain)
            .map_err(|e| format!("assemble: {e}"))?;
        gst::Element::link_many(&chain).map_err(|e| format!("link: {e}"))?;
        Ok((Running::start(pipeline, appsrc, &appsink)?, decoder_handle))
    }

    /// Decode one access unit and return the newest picture that is ready.
    ///
    /// `keyframe_hint` is the transport's keyframe flag where it has one;
    /// `None` makes the decoder read the bitstream. Units before the first
    /// keyframe are dropped without touching the decoder. A decoder element
    /// that fails is replaced by the next candidate, which then waits for
    /// the next keyframe.
    ///
    /// # Errors
    ///
    /// [`RealtimeError::Unavailable`] once every candidate has failed.
    pub fn decode(
        &mut self,
        unit: &[u8],
        keyframe_hint: Option<bool>,
    ) -> Result<Option<DecodedPicture>, RealtimeError> {
        if unit.is_empty() {
            return Ok(None);
        }
        let keyframe = keyframe_hint.unwrap_or(false) || is_keyframe(self.config.codec, unit);
        if self.awaiting_keyframe && !keyframe {
            return Ok(None);
        }
        self.awaiting_keyframe = false;
        loop {
            match self.push_and_pull(unit, keyframe) {
                Ok(sample) => {
                    let picture = sample.and_then(|s| self.picture_from_sample(&s));
                    if picture.is_some() {
                        self.first_output_seen = true;
                    }
                    return Ok(picture);
                }
                Err(e) => {
                    let failed = self.element().map_or("decoder", |e| e.element);
                    tracing::warn!(
                        "media: real-time decoder {failed} failed ({e}); trying the next"
                    );
                    self.tried.push(format!("{failed}: {e}"));
                    self.start_next()?;
                    // A keyframe can start the next element right away (a
                    // hardware decoder that refuses the stream's size or
                    // profile fails on exactly this unit); anything else
                    // waits for the next keyframe.
                    if !keyframe {
                        return Ok(None);
                    }
                    self.awaiting_keyframe = false;
                }
            }
        }
    }

    /// Push one unit into the running element and collect the newest
    /// picture it has.
    fn push_and_pull(
        &mut self,
        unit: &[u8],
        keyframe: bool,
    ) -> Result<Option<gst::Sample>, String> {
        // A monotonically increasing timestamp keeps every element's
        // bookkeeping happy; the caller's own timing travels separately.
        self.pts = self.pts.saturating_add(Duration::from_millis(1));
        let mut buffer = gst_buffer(unit.to_vec(), self.pts);
        if !keyframe && let Some(b) = buffer.get_mut() {
            b.set_flags(gst::BufferFlags::DELTA_UNIT);
        }
        let timeout = if self.first_output_seen {
            DECODE_WAIT
        } else {
            startup_timeout()
        };
        let (running, _) = self.running.as_ref().ok_or("no decoder is running")?;
        let mut newest = None;
        // Anything that arrived late from the previous unit first.
        if let Some(s) = running.drain()?.pop() {
            newest = Some(s);
        }
        running.push(buffer)?;
        if let Some(s) = running.next_sample(timeout)? {
            newest = Some(s);
        }
        if let Some(s) = running.drain()?.pop() {
            newest = Some(s);
        }
        Ok(newest)
    }

    fn picture_from_sample(&self, sample: &gst::Sample) -> Option<DecodedPicture> {
        // The colour of the decoded samples, read where the decoder hands
        // them over (the appsink's caps describe the converted RGB).
        let announced = self.stream_color.lock().ok().and_then(|slot| *slot);
        let color = announced.unwrap_or_else(|| {
            self.decoder_element
                .as_ref()
                .and_then(|d| d.static_pad("src"))
                .and_then(|p| p.current_caps())
                .and_then(|c| gst_video::VideoInfo::from_caps(&c).ok())
                .map(|info| stream_color_from_info(&info))
                .unwrap_or_default()
        });
        // The appsink caps already pin the memory and format each output
        // asks for, so the sample is read as whatever it arrived as.
        let mut frame = crate::gst_frames::frame_from_sample(sample, FrameAcceptance::Native)?;
        if let Some((w, h)) = self.config.display_size {
            frame = super::crop_frame(frame, w, h);
        }
        Some(DecodedPicture { frame, color })
    }
}
