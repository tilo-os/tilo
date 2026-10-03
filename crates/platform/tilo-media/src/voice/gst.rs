//! `GStreamer` voice backend, owned by the privileged local media service.
//! Device identifiers must come from observed broker resources; this module
//! cannot grant consent or authenticate a call.
use super::{
    AudioError, AudioStats, BTreeMap, CaptureQueue, CaptureTimeline, CapturedOpus, Duration,
    FRAME_DURATION, FRAME_SAMPLES, Instant, JitterBuffer, MAX_PACKET_BYTES, MAX_PUBLISHERS,
    PlayoutFrame, PublicationId, ReceivedOpus, SEND_LIFETIME, VecDeque, mix_frames, validate_frame,
};
mod codec;
mod device;
pub use codec::{OpusDecoder, OpusEncoder, test_tone};
pub use device::CaptureStop;
use device::{DevicePipeline, Permit};
use gst::prelude::*;
use gstreamer as gst;
use gstreamer_app as gst_app;
use std::{
    os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd},
    sync::Arc,
};

pub enum CaptureSource<'a> {
    PipeWire {
        node: u32,
        serial: u32,
        /// A one-shot connected socket handed off by the native observer that
        /// verified this exact node/serial. No other protocol reader may remain.
        remote_fd: BorrowedFd<'a>,
    },
}
pub struct ObservedOutput<'a> {
    pub node: u32,
    pub serial: u32,
    /// Same observer custody requirement as `CaptureSource`; an independent
    /// connected socket, never a duplicate of an already active Gst core.
    pub remote_fd: BorrowedFd<'a>,
}
const CAPTURE_STARTUP_TIMEOUT: Duration = Duration::from_secs(2);
struct RunningPipeline(gst::Pipeline);
impl Drop for RunningPipeline {
    fn drop(&mut self) {
        let _ = self.0.set_state(gst::State::Null);
    }
}

struct Capture {
    pipeline: DevicePipeline,
    sink: gst_app::AppSink,
    still_current: Arc<dyn Fn() -> bool + Send + Sync>,
    stop: CaptureStop,
}

pub struct VoiceAudio {
    output: DevicePipeline,
    output_source: gst_app::AppSrc,
    capture: Option<Capture>,
    captured: CaptureQueue,
    closed: bool,
    jitter: JitterBuffer,
    decoders: BTreeMap<PublicationId, Decoder>,
    last_output: Option<Instant>,
    timeline: CaptureTimeline,
}
impl VoiceAudio {
    /// Start the persistent output pipeline on the observer's exact socket.
    ///
    /// # Errors
    ///
    /// [`AudioError::Unavailable`] without a usable `GStreamer`/`PipeWire` stack,
    /// [`AudioError::DeviceLost`] when the socket cannot be retained, [`AudioError::Codec`] when
    /// the pipeline cannot be built, or [`AudioError::Capacity`] without a device worker permit.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "the observed output is a one-shot custody handoff; callers must not reuse it"
    )]
    pub fn new(output: ObservedOutput<'_>) -> Result<Self, AudioError> {
        gst::init().map_err(|_| AudioError::Unavailable)?;
        let pipeline = gst::Pipeline::new();
        let source = gst_app::AppSrc::builder()
            .is_live(true)
            .format(gst::Format::Time)
            .build();
        let convert = element("audioconvert")?;
        let resample = element("audioresample")?;
        let remote_fd = output
            .remote_fd
            .try_clone_to_owned()
            .map_err(|_| AudioError::DeviceLost)?;
        let sink = element("pipewiresink")?;
        configure_pipewire(&sink, output.serial, remote_fd.as_fd())?;
        sink.set_property("sync", true);
        sink.set_property("async", false);
        pipeline
            .add_many([source.upcast_ref(), &convert, &resample, &sink])
            .map_err(|_| AudioError::Codec)?;
        gst::Element::link_many([source.upcast_ref(), &convert, &resample, &sink])
            .map_err(|_| AudioError::Codec)?;
        Self::from_output_bound(pipeline, source, Some(remote_fd))
    }
    #[cfg(test)]
    fn from_output(pipeline: gst::Pipeline, source: gst_app::AppSrc) -> Result<Self, AudioError> {
        Self::from_output_bound(pipeline, source, None)
    }
    fn from_output_bound(
        pipeline: gst::Pipeline,
        source: gst_app::AppSrc,
        remote_fd: Option<OwnedFd>,
    ) -> Result<Self, AudioError> {
        let permit = Permit::acquire()?;
        source.set_caps(Some(&raw_caps()));
        source.set_property("max-buffers", 1u64);
        source.set_property("max-bytes", 1920u64);
        source.set_property("block", false);
        source.set_property_from_str("leaky-type", "downstream");
        let pipeline = DevicePipeline::start(pipeline, remote_fd, None, false, permit)?;
        Ok(Self {
            output: pipeline,
            output_source: source,
            capture: None,
            captured: CaptureQueue::default(),
            closed: false,
            jitter: JitterBuffer::default(),
            decoders: BTreeMap::new(),
            last_output: None,
            timeline: CaptureTimeline::default(),
        })
    }
    /// Enqueue bounded startup. The current-authority callback must be a
    /// nonblocking atomic/expiry check: it also runs on the deadline watchdog.
    ///
    /// # Errors
    ///
    /// [`AudioError::Closed`] after close, [`AudioError::Revoked`] when no longer current, and the
    /// device, codec or capacity error that prevented startup.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "the capture source is a one-shot custody handoff; callers must not reuse it"
    )]
    pub fn start_capture(
        &mut self,
        source: CaptureSource<'_>,
        still_current: Arc<dyn Fn() -> bool + Send + Sync>,
    ) -> Result<(), AudioError> {
        if self.closed {
            return Err(AudioError::Closed);
        }
        if !still_current() {
            return Err(AudioError::Revoked);
        }
        let CaptureSource::PipeWire {
            node: _,
            serial,
            remote_fd,
        } = source;
        let remote_fd = remote_fd
            .try_clone_to_owned()
            .map_err(|_| AudioError::DeviceLost)?;
        let source = element("pipewiresrc")?;
        configure_pipewire(&source, serial, remote_fd.as_fd())?;
        source.set_property("do-timestamp", true);
        self.start_capture_element_bound(&source, still_current, Some(remote_fd))
    }
    #[cfg(test)]
    fn start_capture_element(
        &mut self,
        source: &gst::Element,
        still_current: Arc<dyn Fn() -> bool + Send + Sync>,
    ) -> Result<(), AudioError> {
        self.start_capture_element_bound(source, still_current, None)
    }
    fn start_capture_element_bound(
        &mut self,
        source: &gst::Element,
        still_current: Arc<dyn Fn() -> bool + Send + Sync>,
        remote_fd: Option<OwnedFd>,
    ) -> Result<(), AudioError> {
        if self.closed {
            return Err(AudioError::Closed);
        }
        if !still_current() {
            return Err(AudioError::Revoked);
        }
        // Refusing capacity does not interrupt the existing active capture.
        let permit = Permit::acquire()?;
        let pipeline = gst::Pipeline::new();
        let convert = element("audioconvert")?;
        let resample = element("audioresample")?;
        let caps = gst::ElementFactory::make("capsfilter")
            .property("caps", raw_caps())
            .build()
            .map_err(|_| AudioError::Codec)?;
        let encode = encoder()?;
        let sink = gst_app::AppSink::builder()
            .sync(false)
            .max_buffers(5)
            .build();
        sink.set_property("drop", true);
        pipeline
            .add_many([
                source,
                &convert,
                &resample,
                &caps,
                &encode,
                sink.upcast_ref(),
            ])
            .map_err(|_| AudioError::Codec)?;
        gst::Element::link_many([
            source,
            &convert,
            &resample,
            &caps,
            &encode,
            sink.upcast_ref(),
        ])
        .map_err(|_| AudioError::Codec)?;
        self.stop_capture();
        let pipeline = DevicePipeline::start(
            pipeline,
            remote_fd,
            Some(still_current.clone()),
            true,
            permit,
        )?;
        let stop = pipeline.stop_handle();
        self.capture = Some(Capture {
            pipeline,
            sink,
            still_current,
            stop,
        });
        self.captured.start();
        self.timeline.reset_source();
        Ok(())
    }
    /// Fences capture, shuts down the exact daemon socket, and discards samples
    /// before returning. Driver Null cleanup stays on its bounded worker.
    pub fn stop_capture(&mut self) {
        self.captured.stop();
        self.capture = None;
    }
    /// Collect the fresh complete frames captured since the last poll.
    ///
    /// # Errors
    ///
    /// [`AudioError::Closed`] after close; otherwise a capture failure, revocation or malformed
    /// sample, which also stops capture.
    pub fn poll_capture(&mut self, now: Instant) -> Result<Vec<CapturedOpus>, AudioError> {
        if self.closed {
            return Err(AudioError::Closed);
        }
        let Some(capture) = &mut self.capture else {
            return Ok(Vec::new());
        };
        if let Some(error) = capture.pipeline.failure() {
            self.stop_capture();
            return Err(error);
        }
        if !capture.stop.current() || !(capture.still_current)() {
            self.stop_capture();
            return Err(AudioError::Revoked);
        }
        if let Err(error) = check_bus(&capture.pipeline.pipeline) {
            self.stop_capture();
            return Err(error);
        }
        if !capture.pipeline.ready()
            && now.saturating_duration_since(capture.pipeline.started_at())
                >= CAPTURE_STARTUP_TIMEOUT
        {
            self.stop_capture();
            return Err(AudioError::DeviceLost);
        }
        let Some(running) = capture.pipeline.pipeline.current_running_time() else {
            // Live PipeWire negotiation may not have selected a clock yet.
            if capture.pipeline.ready() {
                self.stop_capture();
                return Err(AudioError::DeviceLost);
            }
            return Ok(Vec::new());
        };
        for _ in 0..5 {
            let Some(sample) = capture.sink.try_pull_sample(gst::ClockTime::ZERO) else {
                break;
            };
            let buffer = sample.buffer().ok_or(AudioError::Codec)?;
            // Encoder priming/trailing clips are not a complete wire frame.
            if buffer.duration() != Some(gst::ClockTime::from_mseconds(20)) {
                continue;
            }
            let pts = buffer.pts().ok_or(AudioError::Codec)?;
            let Some(age) = running.checked_sub(pts) else {
                continue;
            };
            let age = Duration::from_nanos(age.nseconds());
            if age >= SEND_LIFETIME {
                continue;
            }
            let captured_at = now.checked_sub(age).ok_or(AudioError::InvalidFrame)?;
            if buffer.size() > MAX_PACKET_BYTES {
                return Err(AudioError::InvalidFrame);
            }
            let payload = buffer
                .map_readable()
                .map_err(|_| AudioError::Codec)?
                .as_slice()
                .to_vec();
            let timestamp = u64::try_from(u128::from(pts.nseconds()) * 48_000 / 1_000_000_000)
                .map_err(|_| AudioError::InvalidFrame)?;
            let timestamp = self.timeline.timestamp(timestamp, captured_at)?;
            self.captured.push(
                CapturedOpus {
                    timestamp_48k: timestamp,
                    duration_samples: 960,
                    payload,
                    captured_at,
                },
                now,
            )?;
            capture.pipeline.mark_sample();
        }
        if !capture.stop.current() || !(capture.still_current)() {
            self.stop_capture();
            return Err(AudioError::Revoked);
        }
        Ok(self.captured.drain(now))
    }
    /// Admit one received frame for its publisher's decode and playout.
    ///
    /// # Errors
    ///
    /// [`AudioError::Closed`] after close, [`AudioError::Capacity`] when the publisher's queue is
    /// full, or the jitter buffer's refusal.
    pub fn receive(&mut self, frame: ReceivedOpus, now: Instant) -> Result<(), AudioError> {
        if self.closed {
            return Err(AudioError::Closed);
        }
        let id = frame.publisher.clone();
        let outstanding = if let Some(decoder) = self.decoders.get_mut(&id) {
            decoder.expire(now);
            decoder.outstanding()
        } else {
            0
        };
        if outstanding.saturating_add(self.jitter.queued_for(&id)) >= 10 {
            return Err(AudioError::Capacity);
        }
        self.jitter.push(frame, now)?;
        if self.jitter.target_ms(&id).is_some() {
            self.decoders
                .retain(|key, _| key.namespace != id.namespace || *key == id);
        }
        Ok(())
    }
    /// Decode due frames and push at most one mixed output frame.
    ///
    /// # Errors
    ///
    /// [`AudioError::Closed`] after close, an output device failure, or a decoder capacity or
    /// codec failure.
    pub fn playout(&mut self, now: Instant) -> Result<(), AudioError> {
        if self.closed {
            return Err(AudioError::Closed);
        }
        if let Some(error) = self.output.failure() {
            return Err(error);
        }
        if let Err(error) = check_bus(&self.output.pipeline) {
            self.output.stop();
            return Err(error);
        }
        if !self.output.ready() {
            return Ok(());
        }
        for frame in self.jitter.poll(now) {
            if !self.decoders.contains_key(&frame.publisher) {
                if self.decoders.len() >= MAX_PUBLISHERS {
                    return Err(AudioError::Capacity);
                }
                self.decoders
                    .insert(frame.publisher.clone(), Decoder::new()?);
            }
            if let Some(decoder) = self.decoders.get_mut(&frame.publisher) {
                decoder.expire(now);
                if decoder
                    .outstanding()
                    .saturating_add(self.jitter.queued_for(&frame.publisher))
                    >= 10
                {
                    return Err(AudioError::Capacity);
                }
                decoder.push(frame, now)?;
            }
        }
        for decoder in self.decoders.values_mut() {
            decoder.pull(now)?;
        }
        if self
            .last_output
            .is_some_and(|last| now.saturating_duration_since(last) < FRAME_DURATION)
        {
            return Ok(());
        }
        let frames: Vec<Vec<i16>> = self
            .decoders
            .values_mut()
            .filter_map(|decoder| decoder.render(now))
            .collect();
        if frames.is_empty() {
            return Ok(());
        }
        let refs: Vec<&[i16]> = frames.iter().map(Vec::as_slice).collect();
        let mixed = mix_frames(&refs)?;
        let bytes: Vec<u8> = mixed.into_iter().flat_map(i16::to_le_bytes).collect();
        let mut buffer = gst::Buffer::from_slice(bytes);
        let writable = buffer.get_mut().ok_or(AudioError::Codec)?;
        writable.set_pts(
            self.output
                .pipeline
                .current_running_time()
                .unwrap_or(gst::ClockTime::ZERO),
        );
        writable.set_duration(gst::ClockTime::from_mseconds(20));
        self.output_source
            .push_buffer(buffer)
            .map_err(|_| AudioError::DeviceLost)?;
        self.last_output = Some(now);
        Ok(())
    }
    pub fn remove_publisher(&mut self, publisher: &PublicationId) {
        self.jitter.remove(publisher);
        self.decoders.remove(publisher);
    }
    pub fn close(&mut self) {
        self.closed = true;
        self.stop_capture();
        self.jitter.clear();
        self.decoders.clear();
        self.output.stop();
    }
    /// True only after a complete, fresh encoded sample has been observed.
    /// Starting a pipeline or holding a consent lease is not readiness.
    pub fn capture_ready(&self) -> bool {
        !self.closed
            && self
                .capture
                .as_ref()
                .is_some_and(|capture| capture.pipeline.ready() && (capture.still_current)())
    }
    /// The output worker completed Playing under its current exact socket.
    /// This does not claim that a physical speaker is audible.
    pub fn output_ready(&self) -> bool {
        !self.closed && self.output.ready()
    }
    pub fn capture_active(&self) -> bool {
        self.capture_ready()
    }
    pub fn capture_stop_handle(&self) -> Option<CaptureStop> {
        self.capture.as_ref().map(|capture| capture.stop.clone())
    }
    pub fn decoder_count(&self) -> usize {
        self.decoders.len()
    }
    pub fn stats(&self) -> AudioStats {
        self.jitter.stats()
    }
}

impl Drop for VoiceAudio {
    fn drop(&mut self) {
        self.close();
    }
}

fn raw_caps() -> gst::Caps {
    gst::Caps::builder("audio/x-raw")
        .field("format", "S16LE")
        .field("rate", 48_000i32)
        .field("channels", 1i32)
        .field("layout", "interleaved")
        .build()
}
fn opus_caps() -> gst::Caps {
    gst::Caps::builder("audio/x-opus")
        .field("rate", 48_000i32)
        .field("channels", 1i32)
        .field("channel-mapping-family", 0i32)
        .build()
}

struct PcmFrame {
    samples: Vec<i16>,
    expires_at: Instant,
}
struct Decoder {
    pipeline: RunningPipeline,
    source: gst_app::AppSrc,
    sink: gst_app::AppSink,
    pending: BTreeMap<u64, Instant>,
    ready: VecDeque<PcmFrame>,
    next_pts: u64,
}
impl Decoder {
    fn new() -> Result<Self, AudioError> {
        let pipeline = RunningPipeline(gst::Pipeline::new());
        let source = gst_app::AppSrc::builder()
            .is_live(true)
            .format(gst::Format::Time)
            .build();
        source.set_caps(Some(&opus_caps()));
        source.set_property("max-buffers", 10u64);
        source.set_property("max-bytes", 40_640u64);
        source.set_property("block", false);
        source.set_property_from_str("leaky-type", "downstream");
        let decode = gst::ElementFactory::make("opusdec")
            .property("plc", true)
            .property("max-errors", 0i32)
            .build()
            .map_err(|_| AudioError::Codec)?;
        let convert = element("audioconvert")?;
        let sink = gst_app::AppSink::builder()
            .sync(false)
            .max_buffers(10)
            .caps(&raw_caps())
            .build();
        sink.set_property("drop", true);
        pipeline
            .0
            .add_many([source.upcast_ref(), &decode, &convert, sink.upcast_ref()])
            .map_err(|_| AudioError::Codec)?;
        gst::Element::link_many([source.upcast_ref(), &decode, &convert, sink.upcast_ref()])
            .map_err(|_| AudioError::Codec)?;
        pipeline
            .0
            .set_state(gst::State::Playing)
            .map_err(|_| AudioError::Codec)?;
        Ok(Self {
            pipeline,
            source,
            sink,
            pending: BTreeMap::new(),
            ready: VecDeque::new(),
            next_pts: 0,
        })
    }
    /// Codec work in flight plus decoded frames awaiting output.
    fn outstanding(&self) -> usize {
        self.pending.len().saturating_add(self.ready.len())
    }
    fn expire(&mut self, now: Instant) {
        // Pending entries count actual codec work, even after expiry. Retire
        // only when its output is observed; a stalled decoder cannot queue
        // an unbounded stream of serialized PLC events.
        self.ready.retain(|frame| frame.expires_at > now);
    }
    fn render(&mut self, now: Instant) -> Option<Vec<i16>> {
        const SPAN: usize = FRAME_SAMPLES - 1;
        // Every source sample must remain valid for the whole output frame.
        self.ready.retain(|frame| {
            frame
                .expires_at
                .checked_duration_since(now)
                .is_some_and(|left| left >= FRAME_DURATION)
        });
        let available: usize = self.ready.iter().map(|frame| frame.samples.len()).sum();
        // Bound drift correction to one sample per 20ms (about 1042ppm).
        // Backlog is compressed; a one-sample shortfall is interpolated. This
        // preserves fixed-size output without accumulating clock drift.
        let consume = if available >= FRAME_SAMPLES * 2 {
            FRAME_SAMPLES + 1
        } else if available >= FRAME_SAMPLES {
            FRAME_SAMPLES
        } else if available == FRAME_SAMPLES - 1 {
            FRAME_SAMPLES - 1
        } else {
            return None;
        };
        let mut input = Vec::with_capacity(consume);
        while input.len() < consume {
            let front = self.ready.front_mut()?;
            let take = consume.saturating_sub(input.len()).min(front.samples.len());
            input.extend(front.samples.drain(..take));
            if front.samples.is_empty() {
                self.ready.pop_front();
            }
        }
        if consume == FRAME_SAMPLES {
            return Some(input);
        }
        // `consume` is one sample off FRAME_SAMPLES here, so every index and
        // weighted sum below is small; a failed step would only drop the frame.
        let last = consume.checked_sub(1)?;
        let denominator = i64::try_from(SPAN).ok()?;
        (0..FRAME_SAMPLES)
            .map(|index| {
                let position = index.checked_mul(last)?;
                let left = position / SPAN;
                let fraction = i64::try_from(position % SPAN).ok()?;
                let a = i64::from(*input.get(left)?);
                let b = i64::from(*input.get(left.checked_add(1)?.min(last))?);
                let weighted = a
                    .checked_mul(denominator.checked_sub(fraction)?)?
                    .checked_add(b.checked_mul(fraction)?)?;
                i16::try_from(weighted.checked_div(denominator)?).ok()
            })
            .collect()
    }
    fn push(&mut self, frame: PlayoutFrame, now: Instant) -> Result<(), AudioError> {
        self.expire(now);
        if frame.expires_at <= now {
            return Ok(());
        }
        if self.outstanding() >= 10 {
            return Err(AudioError::Capacity);
        }
        let pts = self.next_pts;
        self.next_pts = pts
            .checked_add(20_000_000)
            .ok_or(AudioError::InvalidFrame)?;
        self.pending.insert(pts, frame.expires_at);
        if let Some(payload) = frame.payload {
            let mut buffer = gst::Buffer::from_slice(payload);
            let writable = buffer.get_mut().ok_or(AudioError::Codec)?;
            writable.set_pts(gst::ClockTime::from_nseconds(pts));
            writable.set_duration(gst::ClockTime::from_mseconds(20));
            self.source
                .push_buffer(buffer)
                .map_err(|_| AudioError::Codec)?;
        } else {
            // GstAudioDecoder discards empty buffers. Its serialized GAP event
            // path invokes opusdec PLC; AppSrc queues it behind preceding data.
            if !self.source.send_event(gst::event::Gap::new(
                gst::ClockTime::from_nseconds(pts),
                gst::ClockTime::from_mseconds(20),
            )) {
                return Err(AudioError::Codec);
            }
        }
        Ok(())
    }
    fn pull(&mut self, now: Instant) -> Result<(), AudioError> {
        self.expire(now);
        check_bus(&self.pipeline.0).map_err(|_| AudioError::Codec)?;
        for _ in 0..10 {
            let Some(sample) = self.sink.try_pull_sample(gst::ClockTime::ZERO) else {
                break;
            };
            let buffer = sample.buffer().ok_or(AudioError::Codec)?;
            let pts = buffer.pts().ok_or(AudioError::Codec)?.nseconds();
            let Some(expires_at) = self.pending.remove(&pts) else {
                continue;
            };
            if expires_at <= now {
                continue;
            }
            let mapped = buffer.map_readable().map_err(|_| AudioError::Codec)?;
            if mapped.size() != FRAME_SAMPLES * 2 {
                return Err(AudioError::InvalidFrame);
            }
            let samples = mapped
                .as_slice()
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| i16::from_le_bytes(*pair))
                .collect();
            if self.ready.len() >= 10 {
                return Err(AudioError::Capacity);
            }
            self.ready.push_back(PcmFrame {
                samples,
                expires_at,
            });
        }
        Ok(())
    }
}
fn element(name: &str) -> Result<gst::Element, AudioError> {
    gst::ElementFactory::make(name)
        .build()
        .map_err(|_| AudioError::Codec)
}
fn check_bus(pipeline: &gst::Pipeline) -> Result<(), AudioError> {
    let bus = pipeline.bus().ok_or(AudioError::DeviceLost)?;
    if bus
        .pop_filtered(&[gst::MessageType::Error, gst::MessageType::Eos])
        .is_some()
    {
        return Err(AudioError::DeviceLost);
    }
    Ok(())
}

fn encoder() -> Result<gst::Element, AudioError> {
    gst::ElementFactory::make("opusenc")
        .property("bitrate", 32_000i32)
        .property_from_str("audio-type", "voice")
        .property_from_str("frame-size", "20")
        .build()
        .map_err(|_| AudioError::Codec)
}

/// Configure while Null. Numeric `path` is deliberately never used: only
/// `target-object` resolves object.serial. The owner supplies the exact daemon
/// socket; fd=-1 would silently reconnect by name and is never permitted.
fn configure_pipewire(
    element: &gst::Element,
    serial: u32,
    fd: BorrowedFd<'_>,
) -> Result<(), AudioError> {
    let required = [
        ("target-object", String::static_type()),
        ("path", String::static_type()),
        ("stream-properties", gst::Structure::static_type()),
        ("fd", i32::static_type()),
    ];
    for (name, value_type) in required {
        let property = element.find_property(name).ok_or(AudioError::Unavailable)?;
        if property.value_type() != value_type
            || !property.flags().contains(gst::glib::ParamFlags::READWRITE)
        {
            return Err(AudioError::Unavailable);
        }
    }
    if element.current_state() != gst::State::Null
        || element.property::<Option<String>>("path").is_some()
    {
        return Err(AudioError::Unavailable);
    }
    let properties = gst::Structure::builder("properties")
        .field("node.dont-fallback", true)
        .field("node.dont-reconnect", true)
        .field("node.dont-move", true)
        .field("node.linger", false)
        .build();
    let target = serial.to_string();
    element.set_property("target-object", &target);
    element.set_property("stream-properties", &properties);
    element.set_property("fd", fd.as_raw_fd());
    if element
        .property::<Option<String>>("target-object")
        .as_deref()
        != Some(target.as_str())
        || element
            .property::<Option<gst::Structure>>("stream-properties")
            .as_ref()
            != Some(&properties)
        || element.property::<i32>("fd") != fd.as_raw_fd()
        || element.property::<Option<String>>("path").is_some()
    {
        return Err(AudioError::Unavailable);
    }
    // PipeWire >=1.6 can raise an immediate bus error on source disconnect.
    // Older sources still get DONT_RECONNECT and the native owner loss fence.
    if let Some(property) = element.find_property("on-disconnect") {
        if !property.flags().contains(gst::glib::ParamFlags::READWRITE) {
            return Err(AudioError::Unavailable);
        }
        let class = gst::glib::EnumClass::with_type(property.value_type())
            .ok_or(AudioError::Unavailable)?;
        let error = class
            .to_value_by_nick("error")
            .ok_or(AudioError::Unavailable)?;
        element.set_property_from_value("on-disconnect", &error);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::voice::RECEIVE_LIFETIME;
    use std::sync::atomic::{AtomicBool, Ordering};
    static DEVICE_TESTS: std::sync::Mutex<()> = std::sync::Mutex::new(());
    fn device_test_lock() -> std::sync::MutexGuard<'static, ()> {
        let guard = DEVICE_TESTS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        wait_for_device_workers();
        guard
    }

    mod delayed {
        use super::*;
        use gst::glib;
        use gst::subclass::prelude::*;
        #[derive(Default)]
        pub struct Source {
            pub gate: std::sync::Mutex<Option<Arc<Gate>>>,
        }
        #[derive(Default)]
        pub struct Gate {
            pub entered: AtomicBool,
            released: std::sync::Mutex<bool>,
            wake: std::sync::Condvar,
        }
        impl Gate {
            pub fn release(&self) {
                *self.released.lock().unwrap() = true;
                self.wake.notify_all();
            }
        }
        impl Drop for Gate {
            fn drop(&mut self) {
                self.release();
            }
        }
        #[gst::glib::object_subclass]
        impl ObjectSubclass for Source {
            const NAME: &'static str = "TiloDelayedVoiceSourceTest";
            type Type = super::DelayedSource;
            type ParentType = gst::Bin;
        }
        impl ObjectImpl for Source {}
        impl GstObjectImpl for Source {}
        impl BinImpl for Source {}
        impl ElementImpl for Source {
            fn change_state(
                &self,
                transition: gst::StateChange,
            ) -> Result<gst::StateChangeSuccess, gst::StateChangeError> {
                let gate = if transition == gst::StateChange::NullToReady {
                    self.gate.lock().unwrap().clone()
                } else {
                    None
                };
                if let Some(gate) = gate {
                    gate.entered.store(true, Ordering::Release);
                    let released = gate.released.lock().unwrap();
                    let _ = gate
                        .wake
                        .wait_timeout_while(released, Duration::from_secs(4), |released| !*released)
                        .unwrap();
                }
                self.parent_change_state(transition)
            }
        }
    }
    gst::glib::wrapper! {
        pub struct DelayedSource(ObjectSubclass<delayed::Source>) @extends gst::Bin, gst::Element, gst::Object;
    }
    fn delayed_source() -> (gst::Element, Arc<delayed::Gate>) {
        delayed_element(false)
    }
    fn delayed_element(output: bool) -> (gst::Element, Arc<delayed::Gate>) {
        use gst::subclass::prelude::ObjectSubclassIsExt;
        gst::init().unwrap();
        let bin: DelayedSource = gst::glib::Object::new();
        let gate = Arc::new(delayed::Gate::default());
        *bin.imp().gate.lock().unwrap() = Some(gate.clone());
        let source = if output {
            gst::ElementFactory::make("fakesink")
                .property("async", false)
                .build()
                .unwrap()
        } else {
            gst::ElementFactory::make("audiotestsrc")
                .property("is-live", true)
                .build()
                .unwrap()
        };
        bin.add(&source).unwrap();
        let pad = gst::GhostPad::with_target(
            &source
                .static_pad(if output { "sink" } else { "src" })
                .unwrap(),
        )
        .unwrap();
        bin.add_pad(&pad).unwrap();
        (bin.upcast(), gate)
    }

    fn wait_for_worker_exit(stop: &CaptureStop) {
        tilo_test_env::wait_until("the worker to finish actual Null cleanup", || {
            stop.finished()
        });
    }
    fn wait_for_device_workers() {
        tilo_test_env::wait_until("device workers of earlier tests to exit", || {
            device::outstanding_workers() == 0
        });
    }
    fn wait_for_gate(gate: &delayed::Gate) {
        tilo_test_env::wait_until("the real Gst state transition to be entered", || {
            gate.entered.load(Ordering::Acquire)
        });
    }
    fn pending_output() -> (
        VoiceAudio,
        Arc<delayed::Gate>,
        std::os::unix::net::UnixStream,
    ) {
        gst::init().unwrap();
        let (fd, peer) = std::os::unix::net::UnixStream::pair().unwrap();
        let (sink, gate) = delayed_element(true);
        let pipeline = gst::Pipeline::new();
        let source = gst_app::AppSrc::builder()
            .is_live(true)
            .format(gst::Format::Time)
            .build();
        pipeline.add_many([source.upcast_ref(), &sink]).unwrap();
        source.link(&sink).unwrap();
        let audio = VoiceAudio::from_output_bound(pipeline, source, Some(fd.into())).unwrap();
        wait_for_gate(&gate);
        (audio, gate, peer)
    }

    #[test]
    fn delayed_device_start_does_not_block_control_or_publish_late_capture() {
        use std::io::Read;
        let _device_test = device_test_lock();
        let mut audio = software_output();
        let (source, gate) = delayed_source();
        let observed = source.clone();
        let (fd, mut peer) = std::os::unix::net::UnixStream::pair().unwrap();
        let (done, completed) = std::sync::mpsc::channel();
        let caller = std::thread::spawn(move || {
            let result =
                audio.start_capture_element_bound(&source, Arc::new(|| true), Some(fd.into()));
            done.send((audio, result)).unwrap();
        });
        wait_for_gate(&gate);
        let early = completed.recv_timeout(Duration::from_millis(100));
        let nonblocking = early.is_ok();
        if !nonblocking {
            gate.release();
        }
        let (mut audio, result) = early
            .unwrap_or_else(|_| tilo_test_env::recv("the capture start to complete", &completed));
        caller.join().unwrap();
        result.unwrap();
        let stop = audio.capture_stop_handle().unwrap();
        let now = Instant::now();
        audio.stop_capture();
        assert!(
            now.elapsed() < Duration::from_millis(100),
            "cancel waited for driver state lock"
        );
        peer.set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        assert_eq!(
            peer.read(&mut [0]).unwrap(),
            0,
            "exact socket was not shut down"
        );
        assert!(!audio.capture_ready());
        assert!(
            !stop.finished(),
            "deliberately blocked worker must retain its permit"
        );
        gate.release();
        wait_for_worker_exit(&stop);
        assert_eq!(observed.current_state(), gst::State::Null);
        assert!(audio.poll_capture(Instant::now()).unwrap().is_empty());
        assert!(
            nonblocking,
            "set_state blocked the caller on delayed source startup"
        );
    }

    #[test]
    fn device_output_ready_requires_completed_playing_transition() {
        let _device_test = device_test_lock();
        gst::init().unwrap();
        let source = gst_app::AppSrc::builder().format(gst::Format::Time).build();
        let sink = element("fakesink").unwrap();
        let pipeline = gst::Pipeline::new();
        pipeline.add_many([source.upcast_ref(), &sink]).unwrap();
        source.link(&sink).unwrap();
        let audio = VoiceAudio::from_output(pipeline, source).unwrap();
        tilo_test_env::wait_until("the output pipeline to leave Null", || {
            audio.output.pipeline.current_state() != gst::State::Null
        });
        let (_, state, pending) = audio
            .output
            .pipeline
            .state(gst::ClockTime::from_mseconds(100));
        assert_ne!(state, gst::State::Playing);
        assert_eq!(pending, gst::State::Playing);
        assert!(
            !audio.output_ready(),
            "requested async Playing is still pending preroll"
        );
        let mut frame = gst::Buffer::from_slice(vec![0u8; 1920]);
        frame.get_mut().unwrap().set_pts(gst::ClockTime::ZERO);
        audio.output_source.push_buffer(frame).unwrap();
        tilo_test_env::wait_until("the output to become ready", || audio.output_ready());
        assert_eq!(audio.output.pipeline.current_state(), gst::State::Playing);
    }

    #[test]
    fn device_worker_deadline_shuts_socket_without_service_polling() {
        use std::io::Read;
        let _device_test = device_test_lock();
        wait_for_device_workers();
        let (mut audio, gate, mut peer) = pending_output();
        let stop = audio.output.stop_handle();
        assert!(!audio.output_ready());
        peer.set_read_timeout(Some(Duration::from_millis(2300)))
            .unwrap();
        let start = Instant::now();
        assert_eq!(
            peer.read(&mut [0]).unwrap(),
            0,
            "watchdog must revoke without a service tick"
        );
        assert!(start.elapsed() < Duration::from_millis(2300));
        assert!(!stop.finished(), "driver is deliberately still blocked");
        assert!(!audio.output_ready());
        assert_eq!(audio.playout(Instant::now()), Err(AudioError::DeviceLost));
        gate.release();
        wait_for_worker_exit(&stop);
        assert_eq!(audio.output.pipeline.current_state(), gst::State::Null);
    }

    #[test]
    fn device_worker_capacity_survives_dropped_recreated_audio_owners() {
        use std::io::Read;
        let _device_test = device_test_lock();
        wait_for_device_workers();
        let mut pending = Vec::new();
        for _ in 0..device::MAX_DEVICE_WORKERS {
            let (mut audio, gate, mut peer) = pending_output();
            let stop = audio.output.stop_handle();
            audio.close();
            drop(audio);
            peer.set_read_timeout(Some(Duration::from_millis(100)))
                .unwrap();
            assert_eq!(peer.read(&mut [0]).unwrap(), 0);
            assert!(!stop.finished());
            pending.push((gate, stop));
        }
        let denied =
            VoiceAudio::from_output(gst::Pipeline::new(), gst_app::AppSrc::builder().build());
        assert!(matches!(denied, Err(AudioError::Capacity)));
        assert_eq!(device::outstanding_workers(), device::MAX_DEVICE_WORKERS);
        for (gate, _) in &pending {
            gate.release();
        }
        for (_, stop) in &pending {
            wait_for_worker_exit(stop);
        }
        wait_for_device_workers();
        let fresh = software_output();
        assert!(
            fresh.output_ready(),
            "capacity is reclaimed only after real cleanup"
        );
    }

    #[test]
    fn device_worker_capacity_refusal_preserves_existing_active_capture() {
        let _device_test = device_test_lock();
        wait_for_device_workers();
        let mut audio = software_output();
        let source = gst::ElementFactory::make("audiotestsrc")
            .property("is-live", true)
            .build()
            .unwrap();
        audio
            .start_capture_element(&source, Arc::new(|| true))
            .unwrap();
        wait_for_capture(&mut audio);
        let original = audio.capture_stop_handle().unwrap();
        let mut pending = Vec::new();
        for _ in 2..device::MAX_DEVICE_WORKERS {
            let (other, gate, _peer) = pending_output();
            pending.push((other, gate));
        }
        let source = gst::ElementFactory::make("audiotestsrc")
            .property("is-live", true)
            .build()
            .unwrap();
        assert_eq!(
            audio.start_capture_element(&source, Arc::new(|| true)),
            Err(AudioError::Capacity)
        );
        assert!(original.current());
        assert!(audio.capture_ready());
        for (other, gate) in &mut pending {
            other.close();
            gate.release();
        }
    }

    fn software_output() -> VoiceAudio {
        gst::init().unwrap();
        let pipeline = gst::parse::launch(
            "appsrc name=src is-live=true format=time ! fakesink sync=false async=false",
        )
        .unwrap()
        .downcast::<gst::Pipeline>()
        .unwrap();
        let source = pipeline
            .by_name("src")
            .unwrap()
            .downcast::<gst_app::AppSrc>()
            .unwrap();
        let audio = VoiceAudio::from_output(pipeline, source).unwrap();
        tilo_test_env::wait_until("the software output to start", || audio.output_ready());
        audio
    }

    fn wait_for_capture(audio: &mut VoiceAudio) {
        tilo_test_env::wait_until("a complete frame from the real encoder", || {
            !audio.poll_capture(Instant::now()).unwrap().is_empty()
        });
    }

    #[test]
    fn exact_audio_source_rejects_plugins_without_required_binding_properties() {
        use std::os::fd::AsFd;
        let _device_test = device_test_lock();
        gst::init().unwrap();
        let (fd, _peer) = std::os::unix::net::UnixStream::pair().unwrap();
        for factory in ["fakesrc", "fakesink"] {
            let element = element(factory).unwrap();
            assert_eq!(
                configure_pipewire(&element, 246, fd.as_fd()),
                Err(AudioError::Unavailable)
            );
            assert_eq!(element.current_state(), gst::State::Null);
        }
    }

    #[test]
    fn exact_audio_source_startup_waits_for_a_real_frame_and_times_out_to_null() {
        let _device_test = device_test_lock();
        let mut audio = software_output();
        let source = gst_app::AppSrc::builder()
            .is_live(true)
            .format(gst::Format::Time)
            .build();
        source.set_caps(Some(&raw_caps()));
        let observed = source.clone();
        audio
            .start_capture_element(&source.upcast(), Arc::new(|| true))
            .unwrap();
        let worker = audio.capture_stop_handle().unwrap();
        assert!(
            !audio.capture_active(),
            "requested Playing is not observed microphone audio"
        );
        assert!(audio.poll_capture(Instant::now()).unwrap().is_empty());
        assert!(matches!(
            audio.poll_capture(Instant::now() + Duration::from_secs(2)),
            Err(AudioError::DeviceLost)
        ));
        assert!(!audio.capture_active());
        wait_for_worker_exit(&worker);
        assert_eq!(observed.current_state(), gst::State::Null);
    }

    #[test]
    fn exact_audio_source_pending_without_clock_can_be_revoked_synchronously() {
        let _device_test = device_test_lock();
        let mut audio = software_output();
        let source = gst_app::AppSrc::builder()
            .is_live(true)
            .format(gst::Format::Time)
            .build();
        source.set_caps(Some(&raw_caps()));
        let observed = source.clone();
        audio
            .start_capture_element(&source.upcast(), Arc::new(|| true))
            .unwrap();
        let pipeline = &audio.capture.as_ref().unwrap().pipeline.pipeline;
        assert!(pipeline.set_clock(None::<&gst::Clock>).is_ok());
        assert!(pipeline.current_running_time().is_none());
        assert!(audio.poll_capture(Instant::now()).unwrap().is_empty());
        assert!(!audio.capture_ready());
        let worker = audio.capture_stop_handle().unwrap();
        worker.stop();
        wait_for_worker_exit(&worker);
        assert_eq!(observed.current_state(), gst::State::Null);
        assert!(matches!(
            audio.poll_capture(Instant::now()),
            Err(AudioError::Revoked)
        ));
        assert!(!audio.capture_ready());
    }

    #[test]
    fn exact_audio_source_reports_active_only_after_valid_sample() {
        let _device_test = device_test_lock();
        let mut audio = software_output();
        let source = gst::ElementFactory::make("audiotestsrc")
            .property("is-live", true)
            .build()
            .unwrap();
        audio
            .start_capture_element(&source, Arc::new(|| true))
            .unwrap();
        assert!(!audio.capture_active());
        let deadline = tilo_test_env::hang_deadline();
        loop {
            if !audio.poll_capture(Instant::now()).unwrap().is_empty() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "real encoder produced no complete frame"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(audio.capture_active());
        audio.stop_capture();
        assert!(!audio.capture_active());
    }

    #[test]
    fn stalled_decoder_and_plc_share_the_same_ten_frame_capacity() {
        struct Unblock(gst::Pad, Option<gst::PadProbeId>);
        impl Drop for Unblock {
            fn drop(&mut self) {
                if let Some(probe) = self.1.take() {
                    self.0.remove_probe(probe);
                }
            }
        }
        let _device_test = device_test_lock();
        let mut audio = software_output();
        let now = Instant::now();
        let id = PublicationId {
            namespace: "stalled".into(),
            carrier_generation: 1,
        };
        let decoder = Decoder::new().unwrap();
        let pad = decoder.source.static_pad("src").unwrap();
        let probe = pad
            .add_probe(
                gst::PadProbeType::BLOCK | gst::PadProbeType::BUFFER,
                |_, _| gst::PadProbeReturn::Ok,
            )
            .unwrap();
        let _unblock = Unblock(pad, Some(probe));
        audio.decoders.insert(id.clone(), decoder);
        for sequence in 0..9 {
            audio
                .receive(
                    ReceivedOpus {
                        publisher: id.clone(),
                        sequence,
                        timestamp_48k: if sequence == 0 {
                            0
                        } else {
                            (sequence + 100) * 960
                        },
                        duration_samples: 960,
                        payload: vec![0xf8, 0xff, 0xfe],
                        received_at: now,
                        expires_at: now + RECEIVE_LIFETIME,
                    },
                    now,
                )
                .unwrap();
        }
        audio.playout(now + Duration::from_millis(60)).unwrap();
        audio.playout(now + Duration::from_millis(81)).unwrap();
        assert_eq!(
            audio.playout(now + Duration::from_millis(102)),
            Err(AudioError::Capacity)
        );
        assert!(
            audio.decoders[&id].pending.len()
                + audio.decoders[&id].ready.len()
                + audio.jitter.queued_for(&id)
                <= 10
        );
    }

    #[test]
    fn real_capture_poll_is_bounded_and_timestamps_survive_restart() {
        let _device_test = device_test_lock();
        let mut audio = software_output();
        let mut previous = None;
        for _ in 0..2 {
            let source = gst::ElementFactory::make("audiotestsrc")
                .property("is-live", true)
                .build()
                .unwrap();
            audio
                .start_capture_element(&source, Arc::new(|| true))
                .unwrap();
            let deadline = tilo_test_env::hang_deadline();
            let frames = loop {
                let frames = audio.poll_capture(Instant::now()).unwrap();
                if !frames.is_empty() {
                    break frames;
                }
                assert!(
                    Instant::now() < deadline,
                    "real encoder produced no complete frame"
                );
                std::thread::sleep(Duration::from_millis(5));
            };
            assert!(frames.len() <= 5);
            for frame in frames {
                assert_eq!(frame.timestamp_48k % 960, 0);
                assert!(previous.is_none_or(|last| frame.timestamp_48k > last));
                assert!(Instant::now().duration_since(frame.captured_at) < SEND_LIFETIME);
                previous = Some(frame.timestamp_48k);
            }
            audio.stop_capture();
            assert!(audio.poll_capture(Instant::now()).unwrap().is_empty());
        }
    }

    #[test]
    fn loss_concealment_survives_async_decode_without_extending_receive_lifetime() {
        let _device_test = device_test_lock();
        let mut audio = software_output();
        let now = Instant::now();
        let id = PublicationId {
            namespace: "loss".into(),
            carrier_generation: 1,
        };
        audio
            .receive(
                ReceivedOpus {
                    publisher: id,
                    sequence: 0,
                    timestamp_48k: 0,
                    duration_samples: 960,
                    payload: vec![0xf8, 0xff, 0xfe],
                    received_at: now,
                    expires_at: now + RECEIVE_LIFETIME,
                },
                now,
            )
            .unwrap();
        audio.playout(now + Duration::from_millis(60)).unwrap();
        std::thread::sleep(Duration::from_millis(10));
        audio.playout(now + Duration::from_millis(70)).unwrap();
        assert!(audio.last_output.is_some());
        audio.playout(now + Duration::from_millis(80)).unwrap();
        std::thread::sleep(Duration::from_millis(10));
        audio.playout(now + Duration::from_millis(95)).unwrap();
        assert_eq!(
            audio.last_output,
            Some(now + Duration::from_millis(95)),
            "PLC PCM must reach actual mixer after decoder scheduling delay"
        );
        audio.playout(now + RECEIVE_LIFETIME).unwrap();
        assert_eq!(
            audio.last_output,
            Some(now + Duration::from_millis(95)),
            "PLC cannot outlive original receive window"
        );
    }

    #[test]
    fn real_playout_resamples_bounded_pcm_backlog_and_drops_expired_samples() {
        let _device_test = device_test_lock();
        let mut audio = software_output();
        let now = Instant::now();
        let id = PublicationId {
            namespace: "drift".into(),
            carrier_generation: 1,
        };
        let mut decoder = Decoder::new().unwrap();
        for _ in 0..3 {
            decoder.ready.push_back(PcmFrame {
                samples: vec![1000; 960],
                expires_at: now + RECEIVE_LIFETIME,
            });
        }
        audio.decoders.insert(id.clone(), decoder);
        audio.playout(now).unwrap();
        let remaining: usize = audio.decoders[&id]
            .ready
            .iter()
            .map(|frame| frame.samples.len())
            .sum();
        assert_eq!(
            remaining, 1919,
            "drift correction consumes at most one extra sample per 20ms output"
        );
        audio.playout(now + RECEIVE_LIFETIME).unwrap();
        assert!(audio.decoders[&id].ready.is_empty());
    }

    #[test]
    fn encoder_uses_the_actual_voice_profile() {
        let _device_test = device_test_lock();
        gst::init().unwrap();
        let encoder = encoder().unwrap();
        assert_eq!(encoder.property::<i32>("bitrate"), 32_000);
        assert_eq!(
            encoder
                .property_value("frame-size")
                .serialize()
                .unwrap()
                .as_str(),
            "20"
        );
        assert_eq!(
            encoder
                .property_value("audio-type")
                .serialize()
                .unwrap()
                .as_str(),
            "voice"
        );
    }

    #[test]
    fn revoked_capture_refuses_source_and_worker_finishes_real_pipeline_cleanup() {
        let _device_test = device_test_lock();
        let mut audio = software_output();
        let (fd, _peer) = std::os::unix::net::UnixStream::pair().unwrap();
        assert_eq!(
            audio.start_capture(
                CaptureSource::PipeWire {
                    node: 42,
                    serial: 246,
                    remote_fd: fd.as_fd()
                },
                Arc::new(|| false)
            ),
            Err(AudioError::Revoked)
        );
        let valid = Arc::new(AtomicBool::new(true));
        let fence = valid.clone();
        let source = gst::ElementFactory::make("audiotestsrc")
            .property("is-live", true)
            .build()
            .unwrap();
        let observed_source = source.clone();
        audio
            .start_capture_element(&source, Arc::new(move || fence.load(Ordering::Acquire)))
            .unwrap();
        let worker = audio.capture_stop_handle().unwrap();
        wait_for_capture(&mut audio);
        assert!(audio.capture_active());
        valid.store(false, Ordering::Release);
        assert!(matches!(
            audio.poll_capture(Instant::now()),
            Err(AudioError::Revoked)
        ));
        assert!(!audio.capture_active());
        wait_for_worker_exit(&worker);
        assert_eq!(observed_source.current_state(), gst::State::Null);
        assert!(audio.poll_capture(Instant::now()).unwrap().is_empty());
    }

    #[test]
    fn close_stops_all_devices_and_cannot_be_reopened() {
        let _device_test = device_test_lock();
        let mut audio = software_output();
        let (fd, _peer) = std::os::unix::net::UnixStream::pair().unwrap();
        let source = gst::ElementFactory::make("audiotestsrc")
            .property("is-live", true)
            .build()
            .unwrap();
        let observed_source = source.clone();
        audio
            .start_capture_element(&source, Arc::new(|| true))
            .unwrap();
        let worker = audio.capture_stop_handle().unwrap();
        audio.close();
        wait_for_worker_exit(&worker);
        assert_eq!(observed_source.current_state(), gst::State::Null);
        assert_eq!(audio.playout(Instant::now()), Err(AudioError::Closed));
        assert_eq!(
            audio.start_capture(
                CaptureSource::PipeWire {
                    node: 42,
                    serial: 246,
                    remote_fd: fd.as_fd()
                },
                Arc::new(|| true)
            ),
            Err(AudioError::Closed)
        );
    }

    #[test]
    fn independent_stop_handle_stops_capture_without_outer_lock_and_fences_old_generation() {
        let _device_test = device_test_lock();
        let mut audio = software_output();
        let first = gst::ElementFactory::make("audiotestsrc")
            .property("is-live", true)
            .build()
            .unwrap();
        let observed = first.clone();
        audio
            .start_capture_element(&first, Arc::new(|| true))
            .unwrap();
        let stop = audio.capture_stop_handle().unwrap();
        let audio = std::sync::Mutex::new(audio);
        let mut held = audio.lock().unwrap();
        let (done, received) = std::sync::mpsc::channel();
        let worker = std::thread::spawn({
            let stop = stop.clone();
            move || {
                stop.stop();
                let _ = done.send(());
            }
        });
        let result = received.recv_timeout(tilo_test_env::hang_bound());
        if result.is_err() {
            drop(held);
            panic!("capture stop blocked on outer backend lock");
        }
        worker.join().unwrap();
        wait_for_worker_exit(&stop);
        assert_eq!(observed.current_state(), gst::State::Null);
        assert!(!held.capture_active());
        assert!(matches!(
            held.poll_capture(Instant::now()),
            Err(AudioError::Revoked)
        ));
        let second = gst::ElementFactory::make("audiotestsrc")
            .property("is-live", true)
            .build()
            .unwrap();
        held.start_capture_element(&second, Arc::new(|| true))
            .unwrap();
        stop.stop();
        wait_for_capture(&mut held);
        assert!(
            held.capture_active(),
            "a retired lease cannot stop its replacement"
        );
        held.close();
    }

    #[test]
    fn separate_publishers_decode_to_one_bounded_pcm_output_and_reset_on_reconnect() {
        let _device_test = device_test_lock();
        gst::init().unwrap();
        let pipeline = gst::parse::launch("appsrc name=src is-live=true format=time ! appsink name=sink sync=false async=false max-buffers=1 drop=true").unwrap().downcast::<gst::Pipeline>().unwrap();
        let source = pipeline
            .by_name("src")
            .unwrap()
            .downcast::<gst_app::AppSrc>()
            .unwrap();
        let sink = pipeline
            .by_name("sink")
            .unwrap()
            .downcast::<gst_app::AppSink>()
            .unwrap();
        let mut audio = VoiceAudio::from_output(pipeline, source).unwrap();
        let now = Instant::now();
        let frame = |name: &str, generation| ReceivedOpus {
            publisher: PublicationId {
                namespace: name.into(),
                carrier_generation: generation,
            },
            sequence: 0,
            timestamp_48k: 0,
            duration_samples: 960,
            payload: vec![0xf8, 0xff, 0xfe],
            received_at: now,
            expires_at: now + Duration::from_millis(200),
        };
        audio.receive(frame("alice", 1), now).unwrap();
        audio.receive(frame("bob", 1), now).unwrap();
        audio.playout(now + Duration::from_millis(60)).unwrap();
        let deadline = tilo_test_env::hang_deadline();
        let sample = loop {
            audio.playout(now + Duration::from_millis(65)).unwrap();
            if let Some(sample) = sink.try_pull_sample(gst::ClockTime::ZERO) {
                break sample;
            }
            assert!(
                Instant::now() < deadline,
                "real Opus decoder produced no PCM"
            );
            std::thread::sleep(Duration::from_millis(1));
        };
        assert_eq!(sample.buffer().unwrap().size(), 1920);
        assert_eq!(audio.decoder_count(), 2);
        audio.receive(frame("alice", 2), now).unwrap();
        assert_eq!(
            audio.decoder_count(),
            1,
            "old publisher decoder removed before replacement"
        );
        assert_eq!(
            audio.receive(frame("alice", 1), now),
            Err(AudioError::Revoked)
        );
        audio.remove_publisher(&PublicationId {
            namespace: "bob".into(),
            carrier_generation: 1,
        });
        assert_eq!(audio.decoder_count(), 0);
        audio.close();
        assert_eq!(
            audio.receive(frame("alice", 2), now),
            Err(AudioError::Closed)
        );
    }
}
