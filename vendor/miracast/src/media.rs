//! The media path over GStreamer: capture, H.264 encoding, AAC or WFD LPCM
//! audio, MPEG-TS and RTP.
//!
//! With AAC (or no audio) the stream is muxed by `mpegtsmux` and packetized
//! by `rtpmp2tpay`. With LPCM, GStreamer captures and encodes and the
//! crate's own [`crate::ts::TsMuxer`] writes the WFD layout.
//!
//! H.264 encoders are tried in order until one produces a frame: hardware
//! first (VA-API, NVENC, V4L2), then software. Which exist depends on the
//! system's GStreamer plugins.

use std::fmt;
use std::net::{SocketAddr, UdpSocket};
use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;

use crate::params::AudioCodec;
use crate::source::{Media, MediaError, MediaFactory, MediaPlan};
use crate::ts::{RtpPacketizer, Ticks90k, TsMuxer};

/// H.264 encoders in the order they are tried: hardware, then software.
pub const DEFAULT_ENCODERS: &[&str] = &[
    "vah264lpenc",
    "vah264enc",
    "nvh264enc",
    "v4l2h264enc",
    "openh264enc",
    "x264enc",
];

/// AAC encoders in the order they are tried.
const AAC_ENCODERS: &[&str] = &["fdkaacenc", "avenc_aac", "voaacenc"];

/// mpegtsmux pad names choose the PIDs: video 0x1011, audio 0x1100.
const VIDEO_PAD: &str = "sink_4113";
const AUDIO_PAD: &str = "sink_4352";

/// How far presentation runs ahead of the clock reference in the LPCM path,
/// so audio that arrives slightly after its video is still on time.
const PRESENTATION_DELAY: Ticks90k = 18_000;

/// Where the picture comes from.
#[derive(Debug)]
pub enum VideoInput {
    /// A PipeWire node, reached through a connection to PipeWire: what the
    /// ScreenCast portal's `OpenPipeWireRemote` and `Start` grant.
    PipeWire {
        /// The connection to PipeWire.
        remote: OwnedFd,
        /// The node to read.
        node: u32,
    },
    /// A moving test pattern, produced live.
    TestPattern,
}

/// Where the sound comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AudioInput {
    /// What the default output plays, through PulseAudio or PipeWire's
    /// PulseAudio server.
    DefaultMonitor,
    /// A PulseAudio source by name.
    Pulse(String),
    /// A test tone.
    TestTone,
}

/// How the GStreamer media path captures and encodes.
#[derive(Debug)]
pub struct GstMediaConfig {
    /// The picture.
    pub video: VideoInput,
    /// The sound, used when the session negotiated audio.
    pub audio: AudioInput,
    /// The video bitrate, kbit/s.
    pub bitrate_kbps: u32,
    /// H.264 encoders to try, in order; empty means [`DEFAULT_ENCODERS`].
    pub encoders: Vec<String>,
    /// How long an encoder may take to produce its first frame.
    pub start_timeout: Duration,
}

impl GstMediaConfig {
    /// A configuration for `video` and `audio` at `bitrate_kbps`.
    #[must_use]
    pub fn new(video: VideoInput, audio: AudioInput, bitrate_kbps: u32) -> Self {
        Self {
            video,
            audio,
            bitrate_kbps,
            encoders: Vec::new(),
            start_timeout: Duration::from_secs(10),
        }
    }
}

/// Why the media path could not start.
#[derive(Debug)]
pub struct GstMediaError(String);

impl fmt::Display for GstMediaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for GstMediaError {}

fn error(message: impl Into<String>) -> GstMediaError {
    GstMediaError(message.into())
}

/// Starts GStreamer streams for source sessions.
#[derive(Debug)]
pub struct GstMediaFactory {
    config: GstMediaConfig,
}

impl GstMediaFactory {
    /// A factory with `config`. Initializes GStreamer.
    ///
    /// # Errors
    ///
    /// GStreamer could not be initialized.
    pub fn new(config: GstMediaConfig) -> Result<Self, GstMediaError> {
        gst::init().map_err(|e| error(format!("GStreamer could not start: {e}")))?;
        Ok(Self { config })
    }

    /// The H.264 encoders this system has, in the order they would be tried.
    #[must_use]
    pub fn available_encoders(&self) -> Vec<String> {
        self.encoder_order()
            .into_iter()
            .filter(|name| gst::ElementFactory::find(name).is_some())
            .collect()
    }

    fn encoder_order(&self) -> Vec<String> {
        if self.config.encoders.is_empty() {
            DEFAULT_ENCODERS
                .iter()
                .map(|name| (*name).to_owned())
                .collect()
        } else {
            self.config.encoders.clone()
        }
    }
}

impl MediaFactory for GstMediaFactory {
    fn start(&mut self, plan: &MediaPlan) -> Result<Box<dyn Media>, MediaError> {
        let encoders = self.available_encoders();
        if encoders.is_empty() {
            return Err(Box::new(error(format!(
                "no H.264 encoder is installed (looked for {})",
                self.encoder_order().join(", ")
            ))));
        }
        let mut failures = Vec::new();
        for encoder in &encoders {
            match GstMedia::start(&self.config, plan, encoder) {
                Ok(media) => return Ok(Box::new(media)),
                Err(failure) => failures.push(format!("{encoder}: {failure}")),
            }
        }
        Err(Box::new(error(failures.join("; "))))
    }
}

/// A running GStreamer stream.
pub struct GstMedia {
    pipeline: gst::Pipeline,
    parser: gst::Element,
    encoder: String,
    mux: Option<(Arc<AtomicBool>, JoinHandle<()>)>,
}

impl fmt::Debug for GstMedia {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GstMedia")
            .field("encoder", &self.encoder)
            .finish_non_exhaustive()
    }
}

impl GstMedia {
    fn start(
        config: &GstMediaConfig,
        plan: &MediaPlan,
        encoder: &str,
    ) -> Result<Self, GstMediaError> {
        let pipeline = gst::Pipeline::new();
        let frames = Arc::new(AtomicU64::new(0));
        let built = build(config, plan, encoder, &pipeline, &frames);
        let (parser, mux) = match built {
            Ok(parts) => parts,
            Err(failure) => {
                let _ = pipeline.set_state(gst::State::Null);
                return Err(failure);
            }
        };
        let media = Self {
            pipeline,
            parser,
            encoder: encoder.to_owned(),
            mux,
        };
        media
            .pipeline
            .set_state(gst::State::Playing)
            .map_err(|_| error("the pipeline did not start"))?;
        let bus = media
            .pipeline
            .bus()
            .ok_or_else(|| error("the pipeline has no bus"))?;
        let deadline = Instant::now() + config.start_timeout;
        while frames.load(Ordering::Acquire) == 0 {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(error(format!(
                    "no encoded frame within {:?}",
                    config.start_timeout
                )));
            }
            let message = bus.timed_pop_filtered(
                gst::ClockTime::from_mseconds(
                    u64::try_from(remaining.as_millis().min(50)).unwrap_or(50),
                ),
                &[gst::MessageType::Error, gst::MessageType::Eos],
            );
            if let Some(message) = message {
                return Err(error(describe(&message)));
            }
        }
        Ok(media)
    }

    /// The H.264 encoder in use.
    #[must_use]
    pub fn encoder(&self) -> &str {
        &self.encoder
    }
}

impl Media for GstMedia {
    fn request_idr(&mut self) {
        let event = gst::event::CustomUpstream::new(
            gst::Structure::builder("GstForceKeyUnit")
                .field("all-headers", true)
                .build(),
        );
        if let Some(pad) = self.parser.static_pad("sink") {
            pad.push_event(event);
        }
    }

    fn failure(&mut self) -> Option<String> {
        let bus = self.pipeline.bus()?;
        let message = bus.pop_filtered(&[gst::MessageType::Error, gst::MessageType::Eos])?;
        Some(describe(&message))
    }
}

impl Drop for GstMedia {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
        if let Some((stop, thread)) = self.mux.take() {
            stop.store(true, Ordering::Release);
            let _ = thread.join();
        }
    }
}

fn describe(message: &gst::Message) -> String {
    match message.view() {
        gst::MessageView::Error(failure) => {
            let source = failure
                .src()
                .map(|source| source.name().to_string())
                .unwrap_or_default();
            match failure.debug() {
                Some(debug) => format!("{source}: {} ({debug})", failure.error()),
                None => format!("{source}: {}", failure.error()),
            }
        }
        gst::MessageView::Eos(_) => "the capture ended".to_owned(),
        _ => format!("{:?}", message.type_()),
    }
}

type Mux = Option<(Arc<AtomicBool>, JoinHandle<()>)>;

fn build(
    config: &GstMediaConfig,
    plan: &MediaPlan,
    encoder: &str,
    pipeline: &gst::Pipeline,
    frames: &Arc<AtomicU64>,
) -> Result<(gst::Element, Mux), GstMediaError> {
    let (video_tail, parser) = video_chain(config, plan, encoder, pipeline)?;
    let counter = Arc::clone(frames);
    video_tail
        .static_pad("src")
        .ok_or_else(|| error("the video chain has no output"))?
        .add_probe(gst::PadProbeType::BUFFER, move |_, _| {
            counter.fetch_add(1, Ordering::AcqRel);
            gst::PadProbeReturn::Ok
        });
    if plan.audio == Some(AudioCodec::Lpcm) {
        let mux = lpcm_path(config, plan, pipeline, &video_tail)?;
        return Ok((parser, Some(mux)));
    }
    let tsmux = make("mpegtsmux")?;
    set(&tsmux, "alignment", "7");
    let payloader = make("rtpmp2tpay")?;
    set(&payloader, "pt", "33");
    set(&payloader, "mtu", "1328");
    let udp = udp_sink(plan)?;
    add_link(pipeline, &[&tsmux, &payloader, &udp])?;
    let video_pad = link_to_pad(&video_tail, &tsmux, VIDEO_PAD)?;
    if plan.audio == Some(AudioCodec::Aac) {
        let audio_tail = aac_chain(config, pipeline)?;
        let audio_pad = link_to_pad(&audio_tail, &tsmux, AUDIO_PAD)?;
        hold_until_flowing(&video_pad, &audio_pad);
    }
    Ok((parser, None))
}

/// Holds buffers at `held` until the first buffer reaches `flowing`.
///
/// mpegtsmux lists in its program map only the streams it has caps for, and
/// a live mux starts writing as soon as video arrives. Audio capture and
/// encoding start later than video, so without the hold the stream opens with
/// program maps that carry no audio, audio packets arrive on a PID no map
/// names, and a receiver that reads the map once never plays sound. The
/// audio caps are queued ahead of its first buffer, so once that buffer
/// arrives the mux knows both streams before it writes its first packet.
fn hold_until_flowing(held: &gst::Pad, flowing: &gst::Pad) {
    let Some(hold) = held.add_probe(
        gst::PadProbeType::BLOCK | gst::PadProbeType::BUFFER | gst::PadProbeType::BUFFER_LIST,
        |_, _| gst::PadProbeReturn::Ok,
    ) else {
        return;
    };
    let held = held.clone();
    let hold = Mutex::new(Some(hold));
    flowing.add_probe(
        gst::PadProbeType::BUFFER | gst::PadProbeType::BUFFER_LIST,
        move |_, _| {
            if let Some(hold) = hold.lock().ok().and_then(|mut hold| hold.take()) {
                held.remove_probe(hold);
            }
            gst::PadProbeReturn::Remove
        },
    );
}

fn video_chain(
    config: &GstMediaConfig,
    plan: &MediaPlan,
    encoder_name: &str,
    pipeline: &gst::Pipeline,
) -> Result<(gst::Element, gst::Element), GstMediaError> {
    let fps = plan.mode.fps;
    let source = match &config.video {
        VideoInput::PipeWire { remote, node } => {
            let source = make("pipewiresrc")?;
            set(&source, "fd", &remote.as_raw_fd().to_string());
            set(&source, "path", &node.to_string());
            set(&source, "always-copy", "true");
            set(&source, "do-timestamp", "true");
            // A screen cast sends frames only when the screen changes; the
            // keep-alive repeats the last one so the encoder keeps its rate.
            set(&source, "keepalive-time", &(1000 / fps.max(1)).to_string());
            source
        }
        VideoInput::TestPattern => {
            let source = make("videotestsrc")?;
            set(&source, "is-live", "true");
            set(&source, "pattern", "ball");
            source
        }
    };
    let queue = make("queue")?;
    set(&queue, "leaky", "downstream");
    set(&queue, "max-size-buffers", "4");
    let rate = make("videorate")?;
    set(&rate, "drop-only", "true");
    let convert = make("videoconvert")?;
    let scale = make("videoscale")?;
    set(&scale, "add-borders", "true");
    let raw = capsfilter(
        gst::Caps::builder("video/x-raw")
            .field("width", i32::try_from(plan.mode.width).unwrap_or(1280))
            .field("height", i32::try_from(plan.mode.height).unwrap_or(720))
            .field(
                "framerate",
                gst::Fraction::new(i32::try_from(fps).unwrap_or(30), 1),
            )
            .field("pixel-aspect-ratio", gst::Fraction::new(1, 1))
            .build(),
    )?;
    let convert_out = make("videoconvert")?;
    let encoder = make(encoder_name)?;
    configure_encoder(&encoder, encoder_name, config.bitrate_kbps, fps);
    let profile = capsfilter(
        gst::Caps::builder("video/x-h264")
            .field(
                "profile",
                gst::List::new(["constrained-baseline", "baseline"]),
            )
            .build(),
    )?;
    let parser = make("h264parse")?;
    set(&parser, "config-interval", "-1");
    let stream = capsfilter(
        gst::Caps::builder("video/x-h264")
            .field("stream-format", "byte-stream")
            .field("alignment", "au")
            .build(),
    )?;
    let tail = make("queue")?;
    add_link(
        pipeline,
        &[
            &source,
            &queue,
            &rate,
            &convert,
            &scale,
            &raw,
            &convert_out,
            &encoder,
            &profile,
            &parser,
            &stream,
            &tail,
        ],
    )?;
    Ok((tail, parser))
}

fn configure_encoder(encoder: &gst::Element, name: &str, bitrate_kbps: u32, fps: u32) {
    let kbps = bitrate_kbps.to_string();
    let gop = fps.max(1).to_string();
    match name {
        "x264enc" => {
            set(encoder, "tune", "zerolatency");
            set(encoder, "speed-preset", "veryfast");
            set(encoder, "bitrate", &kbps);
            set(encoder, "key-int-max", &gop);
            set(encoder, "bframes", "0");
        }
        "openh264enc" => {
            set(
                encoder,
                "bitrate",
                &(u64::from(bitrate_kbps) * 1000).to_string(),
            );
            set(encoder, "rate-control", "bitrate");
            set(encoder, "gop-size", &gop);
            set(encoder, "usage-type", "screen");
            set(encoder, "complexity", "low");
        }
        "vah264enc" | "vah264lpenc" => {
            set(encoder, "bitrate", &kbps);
            set(encoder, "rate-control", "cbr");
            set(encoder, "key-int-max", &gop);
            set(encoder, "b-frames", "0");
        }
        "nvh264enc" => {
            set(encoder, "bitrate", &kbps);
            set(encoder, "rc-mode", "cbr");
            set(encoder, "gop-size", &gop);
            set(encoder, "bframes", "0");
            set(encoder, "zerolatency", "true");
        }
        "v4l2h264enc" => {
            set(
                encoder,
                "extra-controls",
                &format!(
                    "controls,video_bitrate={},h264_i_frame_period={gop},repeat_sequence_header=1",
                    u64::from(bitrate_kbps) * 1000
                ),
            );
        }
        _ => {}
    }
}

fn audio_source(config: &GstMediaConfig) -> Result<gst::Element, GstMediaError> {
    Ok(match &config.audio {
        AudioInput::DefaultMonitor => {
            let source = make("pulsesrc")?;
            set(&source, "device", "@DEFAULT_MONITOR@");
            set(&source, "do-timestamp", "true");
            source
        }
        AudioInput::Pulse(device) => {
            let source = make("pulsesrc")?;
            set(&source, "device", device);
            set(&source, "do-timestamp", "true");
            source
        }
        AudioInput::TestTone => {
            let source = make("audiotestsrc")?;
            set(&source, "is-live", "true");
            set(&source, "volume", "0.1");
            source
        }
    })
}

fn aac_chain(
    config: &GstMediaConfig,
    pipeline: &gst::Pipeline,
) -> Result<gst::Element, GstMediaError> {
    let name = AAC_ENCODERS
        .iter()
        .find(|name| gst::ElementFactory::find(name).is_some())
        .ok_or_else(|| {
            error(format!(
                "no AAC encoder is installed (looked for {})",
                AAC_ENCODERS.join(", ")
            ))
        })?;
    let source = audio_source(config)?;
    let queue = make("queue")?;
    let convert = make("audioconvert")?;
    let resample = make("audioresample")?;
    let raw = capsfilter(
        gst::Caps::builder("audio/x-raw")
            .field("rate", 48_000)
            .field("channels", 2)
            .build(),
    )?;
    let encoder = make(name)?;
    set(&encoder, "bitrate", "128000");
    let parser = make("aacparse")?;
    let adts = capsfilter(
        gst::Caps::builder("audio/mpeg")
            .field("mpegversion", 4)
            .field("stream-format", "adts")
            .build(),
    )?;
    let tail = make("queue")?;
    add_link(
        pipeline,
        &[
            &source, &queue, &convert, &resample, &raw, &encoder, &parser, &adts, &tail,
        ],
    )?;
    Ok(tail)
}

enum Unit {
    Video(Vec<u8>, gst::ClockTime),
    Audio(Vec<u8>, gst::ClockTime),
}

fn lpcm_path(
    config: &GstMediaConfig,
    plan: &MediaPlan,
    pipeline: &gst::Pipeline,
    video_tail: &gst::Element,
) -> Result<(Arc<AtomicBool>, JoinHandle<()>), GstMediaError> {
    let (units, received) = mpsc::sync_channel::<Unit>(256);
    let video_sink = app_sink(&units, Unit::Video);
    pipeline
        .add(&video_sink)
        .map_err(|e| error(format!("cannot add the video sink: {e}")))?;
    video_tail
        .link(&video_sink)
        .map_err(|e| error(format!("cannot link the video sink: {e}")))?;

    let source = audio_source(config)?;
    let queue = make("queue")?;
    let convert = make("audioconvert")?;
    let resample = make("audioresample")?;
    let raw = capsfilter(
        gst::Caps::builder("audio/x-raw")
            .field("format", "S16BE")
            .field("rate", 48_000)
            .field("channels", 2)
            .field("layout", "interleaved")
            .build(),
    )?;
    let audio_sink = app_sink(&units, Unit::Audio);
    add_link(
        pipeline,
        &[&source, &queue, &convert, &resample, &raw, &audio_sink],
    )?;

    let socket = UdpSocket::bind(SocketAddr::new(plan.local_ip, plan.source_rtp_port))
        .and_then(|socket| socket.connect(plan.sink_rtp).map(|()| socket))
        .map_err(|e| error(format!("cannot open the RTP socket: {e}")))?;
    let stop = Arc::new(AtomicBool::new(false));
    let stopping = Arc::clone(&stop);
    let thread = std::thread::Builder::new()
        .name("wfd-lpcm-mux".to_owned())
        .spawn(move || mux_lpcm(&received, &socket, &stopping))
        .map_err(|e| error(format!("cannot start the muxer: {e}")))?;
    Ok((stop, thread))
}

fn app_sink(units: &SyncSender<Unit>, wrap: fn(Vec<u8>, gst::ClockTime) -> Unit) -> gst::Element {
    let sink = gst_app::AppSink::builder().sync(false).build();
    let units = Mutex::new(units.clone());
    sink.set_callbacks(
        gst_app::AppSinkCallbacks::builder()
            .new_sample(move |sink| {
                let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                let Some(buffer) = sample.buffer() else {
                    return Ok(gst::FlowSuccess::Ok);
                };
                let Some(pts) = buffer.pts() else {
                    return Ok(gst::FlowSuccess::Ok);
                };
                let Ok(map) = buffer.map_readable() else {
                    return Ok(gst::FlowSuccess::Ok);
                };
                if let Ok(units) = units.lock() {
                    // A full queue means the muxer fell behind: drop rather
                    // than stall capture.
                    let _ = units.try_send(wrap(map.as_slice().to_vec(), pts));
                }
                Ok(gst::FlowSuccess::Ok)
            })
            .build(),
    );
    sink.upcast()
}

fn to_ticks(time: gst::ClockTime) -> Ticks90k {
    time.nseconds() * 9 / 100_000
}

fn mux_lpcm(units: &Receiver<Unit>, socket: &UdpSocket, stop: &AtomicBool) {
    // 10 ms of 48 kHz 16-bit stereo per PES packet.
    const AUDIO_CHUNK: usize = 1920;
    const CHUNK_TICKS: Ticks90k = 900;
    let mut muxer = TsMuxer::new(true);
    let mut rtp = RtpPacketizer::new(0x5746_4421);
    let mut base: Option<Ticks90k> = None;
    let mut pending_audio: Vec<u8> = Vec::new();
    let mut pending_pts: Ticks90k = 0;
    let mut out = Vec::new();
    while !stop.load(Ordering::Acquire) {
        let Ok(unit) = units.recv_timeout(Duration::from_millis(100)) else {
            continue;
        };
        out.clear();
        let mut clock = 0;
        match unit {
            Unit::Video(data, pts) => {
                let ticks = to_ticks(pts);
                let base = *base.get_or_insert(ticks);
                let Some(elapsed) = ticks.checked_sub(base) else {
                    continue;
                };
                clock = elapsed;
                muxer.video(&data, elapsed + PRESENTATION_DELAY, elapsed, &mut out);
            }
            Unit::Audio(data, pts) => {
                // Audio before the first frame has no clock to belong to.
                let Some(elapsed) = base.and_then(|base| to_ticks(pts).checked_sub(base)) else {
                    continue;
                };
                if pending_audio.is_empty() {
                    pending_pts = elapsed;
                }
                pending_audio.extend_from_slice(&data);
                while pending_audio.len() >= AUDIO_CHUNK {
                    let chunk: Vec<u8> = pending_audio.drain(..AUDIO_CHUNK).collect();
                    muxer.lpcm(&chunk, pending_pts + PRESENTATION_DELAY, &mut out);
                    clock = pending_pts;
                    pending_pts += CHUNK_TICKS;
                }
            }
        }
        rtp.packetize(&out, clock, |packet| {
            let _ = socket.send(packet);
        });
    }
}

fn udp_sink(plan: &MediaPlan) -> Result<gst::Element, GstMediaError> {
    let sink = make("udpsink")?;
    set(&sink, "host", &plan.sink_rtp.ip().to_string());
    set(&sink, "port", &plan.sink_rtp.port().to_string());
    set(&sink, "bind-address", &plan.local_ip.to_string());
    set(&sink, "bind-port", &plan.source_rtp_port.to_string());
    set(&sink, "sync", "false");
    set(&sink, "async", "false");
    Ok(sink)
}

fn make(factory: &str) -> Result<gst::Element, GstMediaError> {
    gst::ElementFactory::make(factory)
        .build()
        .map_err(|_| error(format!("the GStreamer element {factory} is not installed")))
}

fn capsfilter(caps: gst::Caps) -> Result<gst::Element, GstMediaError> {
    gst::ElementFactory::make("capsfilter")
        .property("caps", caps)
        .build()
        .map_err(|_| error("the GStreamer element capsfilter is not installed"))
}

/// Sets `property` from its string form when the element has it: plugin
/// versions differ in which properties exist.
fn set(element: &gst::Element, property: &str, value: &str) {
    let Some(spec) = element.find_property(property) else {
        return;
    };
    if let Ok(value) = gst::glib::Value::deserialize(value, spec.value_type()) {
        element.set_property_from_value(property, &value);
    }
}

fn add_link(pipeline: &gst::Pipeline, elements: &[&gst::Element]) -> Result<(), GstMediaError> {
    pipeline
        .add_many(elements.iter().copied())
        .map_err(|e| error(format!("cannot build the pipeline: {e}")))?;
    gst::Element::link_many(elements.iter().copied())
        .map_err(|e| error(format!("cannot link the pipeline: {e}")))
}

fn link_to_pad(
    from: &gst::Element,
    to: &gst::Element,
    pad: &str,
) -> Result<gst::Pad, GstMediaError> {
    let target = to
        .request_pad_simple(pad)
        .ok_or_else(|| error(format!("{} has no pad {pad}", to.name())))?;
    from.static_pad("src")
        .ok_or_else(|| error(format!("{} has no output", from.name())))?
        .link(&target)
        .map_err(|e| error(format!("cannot link {} to {pad}: {e:?}", from.name())))?;
    Ok(target)
}
