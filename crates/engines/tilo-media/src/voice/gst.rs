//! `GStreamer` voice backend, owned by the privileged local media service.
//! Device identifiers must come from observed broker resources; this module
//! cannot grant consent or authenticate a call.
use super::{
    AudioError, AudioStats, BTreeMap, CaptureQueue, CaptureTimeline, CapturedOpus, Duration,
    FRAME_DURATION, FRAME_SAMPLES, HEARD_FOR, HEARD_RUN, Instant, JitterBuffer, MAX_PACKET_BYTES,
    MAX_PUBLISHER_FRAMES, MAX_PUBLISHERS, PlayoutFrame, PublicationId, ReceivedOpus, SEND_LIFETIME,
    VecDeque, mix_frames, validate_frame,
};
mod codec;
mod device;
#[cfg(feature = "test-support")]
pub mod software;
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
        remote_fd: BorrowedFd<'a>,
    },
}
pub struct ObservedOutput<'a> {
    pub node: u32,
    pub serial: u32,
    /// Same observer ownership requirement as `CaptureSource`; an independent
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
    sample_diagnosed: bool,
}

const NEXT_FRAME_WITHIN: Duration = Duration::from_millis(30);

pub struct VoiceAudio {
    output: DevicePipeline,
    output_source: gst_app::AppSrc,
    capture: Option<Capture>,
    captured: CaptureQueue,
    closed: bool,
    jitter: JitterBuffer,
    decoders: BTreeMap<PublicationId, Decoder>,
    /// How loud each publication plays here, in percent, by namespace;
    /// one not named plays as it arrives.
    volumes: BTreeMap<String, u16>,
    last_output: Option<Instant>,
    timeline: CaptureTimeline,
    /// The microphone's speech: contiguous transmitted frames up to the
    /// last one, which DTX silence never adds to.
    capture_run: u32,
    capture_last: Option<Instant>,
    deafened: bool,
}
impl VoiceAudio {
    #[expect(
        clippy::needless_pass_by_value,
        reason = "the observed output is a one-shot ownership handoff; callers must not reuse it"
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
    /// A software output: no driver can hold its transition, so it has no
    /// startup deadline and becomes ready whenever it reaches Playing.
    #[cfg(any(test, feature = "test-support"))]
    fn from_output(pipeline: gst::Pipeline, source: gst_app::AppSrc) -> Result<Self, AudioError> {
        Self::start_output(pipeline, source, None, None)
    }
    fn from_output_bound(
        pipeline: gst::Pipeline,
        source: gst_app::AppSrc,
        remote_fd: Option<OwnedFd>,
    ) -> Result<Self, AudioError> {
        Self::start_output(pipeline, source, Some(CAPTURE_STARTUP_TIMEOUT), remote_fd)
    }
    fn start_output(
        pipeline: gst::Pipeline,
        source: gst_app::AppSrc,
        startup_deadline: Option<Duration>,
        remote_fd: Option<OwnedFd>,
    ) -> Result<Self, AudioError> {
        let permit = Permit::acquire()?;
        source.set_caps(Some(&raw_caps()));
        source.set_property("max-buffers", 1u64);
        source.set_property("max-bytes", 1920u64);
        source.set_property("block", false);
        source.set_property_from_str("leaky-type", "downstream");
        let pipeline =
            DevicePipeline::start(pipeline, startup_deadline, remote_fd, None, false, permit)?;
        Ok(Self {
            output: pipeline,
            output_source: source,
            capture: None,
            captured: CaptureQueue::default(),
            closed: false,
            jitter: JitterBuffer::default(),
            decoders: BTreeMap::new(),
            volumes: BTreeMap::new(),
            last_output: None,
            timeline: CaptureTimeline::default(),
            capture_run: 0,
            capture_last: None,
            deafened: false,
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
        reason = "the capture source is a one-shot ownership handoff; callers must not reuse it"
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
        self.start_capture_element_bound(
            &source,
            still_current,
            Some(CAPTURE_STARTUP_TIMEOUT),
            Some(remote_fd),
        )
    }
    fn start_capture_element_bound(
        &mut self,
        source: &gst::Element,
        still_current: Arc<dyn Fn() -> bool + Send + Sync>,
        startup_deadline: Option<Duration>,
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
        // A PipeWire stream's clock can start at zero before the driver begins, then
        // jump to the graph's accumulated ticks. Keep the capture epoch on the monotonic
        // local clock used for freshness; the source's own PTS still passes through
        // GStreamer's live mapping.
        pipeline.use_clock(Some(&gst::SystemClock::obtain()));
        let convert = element("audioconvert")?;
        let resample = element("audioresample")?;
        let caps = gst::ElementFactory::make("capsfilter")
            .property("caps", raw_caps())
            .build()
            .map_err(|_| AudioError::Codec)?;
        let encode = encoder()?;
        let sink = gst_app::AppSink::builder()
            // A device can deliver encoded data ahead of its presentation
            // clock. Release it at that clock, retaining its original PTS;
            // poll_capture still enforces the unchanged freshness window.
            .sync(true)
            .max_buffers(5)
            .build();
        sink.set_property("drop", true);
        // The call polls encoded frames independently of state transitions. Do not
        // hold that live path at preroll; readiness still requires a complete
        // fresh sample in poll_capture, under the current capture fence.
        sink.set_property("async", false);
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
            startup_deadline,
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
            sample_diagnosed: false,
        });
        self.captured.start();
        self.timeline.reset_source();
        Ok(())
    }
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
        if capture.pipeline.startup_expired(now) {
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
            if !capture.sample_diagnosed {
                capture.sample_diagnosed = true;
                tracing::debug!(
                    pts_ns = pts.nseconds(),
                    running_ns = running.nseconds(),
                    "native voice first complete capture timing"
                );
            }
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
            if !super::needs_transmission(&payload) {
                // DTX: the microphone is live and silent. Receivers play
                // comfort noise until the encoder's next update.
                capture.pipeline.mark_sample();
                continue;
            }
            let timestamp = self.timeline.frame_timestamp(captured_at)?;
            self.capture_run = match self.capture_last {
                Some(last) if captured_at.saturating_duration_since(last) <= NEXT_FRAME_WITHIN => {
                    self.capture_run.saturating_add(1)
                }
                _ => 1,
            };
            self.capture_last = Some(captured_at);
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
    /// A frame that arrives while this machine is still behind on that
    /// publisher is dropped before it is decoded: the call goes on.
    ///
    /// # Errors
    ///
    /// [`AudioError::Closed`] after close, or the jitter buffer's refusal of a
    /// frame past its life, malformed, or for a publisher at its limit.
    pub fn receive(&mut self, frame: ReceivedOpus, now: Instant) -> Result<(), AudioError> {
        if self.closed {
            return Err(AudioError::Closed);
        }
        let id = frame.publisher.clone();
        let held = self
            .decoders
            .get_mut(&id)
            .map_or(0, |decoder| {
                decoder.expire(now);
                decoder.outstanding()
            })
            .saturating_add(self.jitter.queued_for(&id));
        if held >= MAX_PUBLISHER_FRAMES {
            // This machine is already behind on the publisher: the frame is
            // heard late or not at all, and the call is not the thing that
            // ends.
            return Ok(());
        }
        self.jitter.push(frame, now)?;
        if self.jitter.target_ms(&id).is_some() {
            self.decoders
                .retain(|key, _| key.namespace != id.namespace || *key == id);
        }
        Ok(())
    }
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
                    let jitter = &self.jitter;
                    self.decoders
                        .retain(|publisher, _| jitter.contains(publisher));
                }
                if self.decoders.len() >= MAX_PUBLISHERS {
                    continue;
                }
                self.decoders
                    .insert(frame.publisher.clone(), Decoder::new()?);
            }
            if let Some(decoder) = self.decoders.get_mut(&frame.publisher) {
                decoder.expire(now);
                if decoder
                    .outstanding()
                    .saturating_add(self.jitter.queued_for(&frame.publisher))
                    >= MAX_PUBLISHER_FRAMES
                {
                    continue;
                }
                decoder.push(frame, now)?;
            }
        }
        for decoder in self.decoders.values_mut() {
            decoder.pull(now)?;
        }
        let output_at = if let Some(last) = self.last_output {
            let elapsed = now.saturating_duration_since(last);
            if elapsed < FRAME_DURATION {
                return Ok(());
            }
            // Preserve the sample clock across small polling delays. Reset
            // after a missed whole frame instead of replaying outage backlog.
            if elapsed < FRAME_DURATION.saturating_mul(2) {
                last.checked_add(FRAME_DURATION)
                    .ok_or(AudioError::InvalidFrame)?
            } else {
                now
            }
        } else {
            now
        };
        let mut frames: Vec<Vec<i16>> = self
            .decoders
            .iter_mut()
            .filter_map(|(publisher, decoder)| {
                let frame = decoder.render(now)?;
                Some(match self.volumes.get(&publisher.namespace) {
                    Some(&percent) => louder(frame, percent),
                    None => frame,
                })
            })
            .collect();
        // Deafened, the call plays on unheard: undeafening hears it as it
        // is then, never a backlog.
        if self.deafened {
            frames.clear();
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
        self.last_output = Some(output_at);
        Ok(())
    }
    pub fn set_volumes(&mut self, volumes: BTreeMap<String, u16>) {
        self.volumes = volumes;
    }
    pub const fn set_deafened(&mut self, deafened: bool) {
        self.deafened = deafened;
    }
    #[must_use]
    pub const fn deafened(&self) -> bool {
        self.deafened
    }
    pub fn heard(&self, now: Instant) -> Vec<PublicationId> {
        self.jitter.heard(now).cloned().collect()
    }
    /// Whether the microphone is heard talking at `now`: it captures and
    /// sends speech, not DTX silence.
    pub fn capture_heard(&self, now: Instant) -> bool {
        self.capture_ready()
            && self.capture_run >= HEARD_RUN
            && self
                .capture_last
                .is_some_and(|last| now.saturating_duration_since(last) < HEARD_FOR)
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
            // Silence: a gap event keeps the stream's clock moving instead of
            // stalling on a frame that never comes.
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
            if self.ready.len() >= MAX_PUBLISHER_FRAMES {
                continue;
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

// 32 kbit/s 20 ms voice with DTX: in silence the encoder emits a 1-byte "nothing to
// send" packet ([`super::needs_transmission`]) for all but one comfort-noise update
// every 400 ms, so a silent member sends ~2.5 packets a second instead of 50.
// Opus enters DTX only after ~200 ms of inactivity and leaves it on the first active
// frame, so speech onsets are coded normally. In-band FEC stays off: QUIC already
// retransmits, and browser receivers cannot decode it.
fn encoder() -> Result<gst::Element, AudioError> {
    gst::ElementFactory::make("opusenc")
        .property("bitrate", 32_000i32)
        .property_from_str("audio-type", "voice")
        .property_from_str("frame-size", "20")
        .property("dtx", true)
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

fn louder(frame: Vec<i16>, percent: u16) -> Vec<i16> {
    let percent = i32::from(percent);
    frame
        .into_iter()
        .map(|sample| {
            let scaled = i32::from(sample).saturating_mul(percent) / 100;
            i16::try_from(scaled).unwrap_or(if scaled < 0 { i16::MIN } else { i16::MAX })
        })
        .collect()
}
