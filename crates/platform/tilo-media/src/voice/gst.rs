//! `GStreamer` voice backend, owned by the privileged local media service.
//! Device identifiers must come from observed broker resources; this module
//! cannot grant consent or authenticate a call.
use super::{
    AudioError, AudioStats, BTreeMap, CaptureQueue, CaptureTimeline, CapturedOpus, Duration,
    FRAME_DURATION, FRAME_SAMPLES, HEARD_FOR, HEARD_RUN, Instant, JitterBuffer, MAX_PACKET_BYTES,
    MAX_PUBLISHERS, PlayoutFrame, PublicationId, ReceivedOpus, SEND_LIFETIME, VecDeque, mix_frames,
    validate_frame,
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
    sample_diagnosed: bool,
}

/// A transmitted frame this soon after the last continues its speech:
/// one frame duration, with half a frame of capture jitter.
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
    /// Plays silence in place of the call: the person is deafened.
    deafened: bool,
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
    /// A software output: no driver can hold its transition, so it has no
    /// startup deadline and becomes ready whenever it reaches Playing.
    #[cfg(test)]
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
        self.start_capture_element_bound(
            &source,
            still_current,
            Some(CAPTURE_STARTUP_TIMEOUT),
            Some(remote_fd),
        )
    }
    /// A software capture: like [`Self::from_output`], no startup deadline.
    #[cfg(test)]
    fn start_capture_element(
        &mut self,
        source: &gst::Element,
        still_current: Arc<dyn Fn() -> bool + Send + Sync>,
    ) -> Result<(), AudioError> {
        self.start_capture_element_bound(source, still_current, None, None)
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
        // A PipeWire stream's advertised clock can start at zero before the
        // driver begins, then jump to the graph's accumulated ticks. Keep the
        // capture epoch on the monotonic local clock used for freshness; the
        // source's original PTS still passes through GStreamer's live mapping.
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
                    // A voice that gave up its slot in the jitter buffer
                    // gives up its decoder too.
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
        // Keep the playback graph clocked while listening or waiting for remote audio.
        // An idle live appsrc can leave a shared PipeWire graph waiting for its first
        // buffer, including independent capture streams. Empty mixing produces one
        // silent frame; the same frame-duration fence bounds it like decoded audio.
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
    /// Play each publication named in `volumes` (by namespace) at its
    /// percent of as it arrives, the rest as they arrive. Who is heard
    /// talking stays as they speak.
    pub fn set_volumes(&mut self, volumes: BTreeMap<String, u16>) {
        self.volumes = volumes;
    }
    /// Play silence in place of the call (`true`), or play it again.
    pub const fn set_deafened(&mut self, deafened: bool) {
        self.deafened = deafened;
    }
    /// Whether the call plays silence here.
    #[must_use]
    pub const fn deafened(&self) -> bool {
        self.deafened
    }
    /// The publishers heard talking at `now`.
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

/// 32 kbit/s 20 ms voice with DTX: in silence the encoder emits a 1-byte
/// "nothing to send" packet ([`super::needs_transmission`]) for all but one
/// comfort-noise update every 400 ms, so a silent participant sends ~2.5
/// packets a second instead of 50. Opus enters DTX only after ~200 ms of
/// inactivity and leaves it on the first active frame, so speech onsets are
/// coded normally. In-band FEC stays off: native publishers send on QUIC
/// streams, which already retransmit, and browser receivers cannot decode it.
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

/// `frame` at `percent` of its level, clipped.
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

#[cfg(test)]
mod tests {
    use super::*;

    use crate::voice::RECEIVE_LIFETIME;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tilo_lock::MutexRecover as _;
    static DEVICE_TESTS: std::sync::Mutex<()> = std::sync::Mutex::new(());
    /// The shared test hang bound, as a pipeline timeout.
    fn hang_bound() -> gst::ClockTime {
        gst::ClockTime::from_nseconds(
            u64::try_from(tilo_test_env::hang_bound().as_nanos()).unwrap_or(u64::MAX),
        )
    }

    fn device_test_lock() -> std::sync::MutexGuard<'static, ()> {
        let guard = DEVICE_TESTS.lock_recover();
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
            /// The held transition went on, released or past its bound.
            pub left: AtomicBool,
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
                    // Held until released; the bound only keeps a failed test
                    // from leaving the element stuck.
                    let _ = gate
                        .wake
                        .wait_timeout_while(
                            gate.released.lock().unwrap(),
                            tilo_test_env::hang_bound().saturating_mul(2),
                            |released| !*released,
                        )
                        .unwrap();
                    gate.left.store(true, Ordering::Release);
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
            let result = audio.start_capture_element_bound(
                &source,
                Arc::new(|| true),
                None,
                Some(fd.into()),
            );
            done.send((audio, result)).unwrap();
        });
        wait_for_gate(&gate);
        // The source's startup stays held until released below, so the start
        // returns only if it does not wait for the device.
        let (mut audio, result) = tilo_test_env::recv(
            "the capture start to return while the source's startup is held",
            &completed,
        );
        caller.join().unwrap();
        result.unwrap();
        let stop = audio.capture_stop_handle().unwrap();
        audio.stop_capture();
        assert!(
            !gate.left.load(Ordering::Acquire),
            "cancel waited for driver state lock"
        );
        peer.set_read_timeout(Some(tilo_test_env::hang_bound()))
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
        let start = Instant::now();
        let (mut audio, gate, mut peer) = pending_output();
        let stop = audio.output.stop_handle();
        assert!(!audio.output_ready());
        peer.set_read_timeout(Some(tilo_test_env::hang_bound()))
            .unwrap();
        assert_eq!(
            peer.read(&mut [0]).unwrap(),
            0,
            "watchdog must revoke without a service tick"
        );
        assert!(
            start.elapsed() >= CAPTURE_STARTUP_TIMEOUT,
            "revoked before the startup deadline"
        );
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
            peer.set_read_timeout(Some(tilo_test_env::hang_bound()))
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
        wait_for_output(&audio, "the software output to start");
        audio
    }

    fn wait_for_output(audio: &VoiceAudio, what: &str) {
        let failure = tilo_test_env::wait_for(what, || {
            if audio.output_ready() {
                Some(None)
            } else {
                audio.output.failure().map(Some)
            }
        });
        assert_eq!(failure, None, "{what}: the output failed instead");
    }

    fn observed_output() -> (VoiceAudio, gst_app::AppSink) {
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
        let audio = VoiceAudio::from_output(pipeline, source).unwrap();
        wait_for_output(&audio, "the observed output to start");
        (audio, sink)
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
        let (fd, _peer) = std::os::unix::net::UnixStream::pair().unwrap();
        audio
            .start_capture_element_bound(
                &source.upcast(),
                Arc::new(|| true),
                Some(CAPTURE_STARTUP_TIMEOUT),
                Some(fd.into()),
            )
            .unwrap();
        let worker = audio.capture_stop_handle().unwrap();
        tilo_test_env::wait_until("the capture's Playing transition to begin", || {
            observed.current_state() != gst::State::Null
        });
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
            incarnation: 1,
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
            let (polled_at, frames) = loop {
                let polled_at = Instant::now();
                let frames = audio.poll_capture(polled_at).unwrap();
                if !frames.is_empty() {
                    break (polled_at, frames);
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
                assert!(polled_at.duration_since(frame.captured_at) < SEND_LIFETIME);
                previous = Some(frame.timestamp_48k);
            }
            audio.stop_capture();
            assert!(audio.poll_capture(Instant::now()).unwrap().is_empty());
        }
    }

    #[test]
    fn encoded_device_pts_jitter_keeps_fresh_frames_on_the_wire_sample_clock() {
        let _device_test = device_test_lock();
        let mut audio = software_output();
        let source = gst::ElementFactory::make("audiotestsrc")
            .property("is-live", true)
            .build()
            .unwrap();
        audio
            .start_capture_element(&source, Arc::new(|| true))
            .unwrap();
        let sink = audio.capture.as_ref().unwrap().sink.clone();
        let counter = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let observed = counter.clone();
        let pad = sink.static_pad("sink").unwrap();
        let probe = pad
            .add_probe(gst::PadProbeType::BUFFER, move |_, info| {
                if let Some(gst::PadProbeData::Buffer(buffer)) = &mut info.data
                    && buffer.duration() == Some(gst::ClockTime::from_mseconds(20))
                {
                    let index = observed.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let pts = buffer.pts().unwrap().nseconds();
                    let jittered = if index.is_multiple_of(2) {
                        pts + 1_000_000
                    } else {
                        pts.saturating_sub(1_000_000)
                    };
                    buffer
                        .make_mut()
                        .set_pts(gst::ClockTime::from_nseconds(jittered));
                }
                gst::PadProbeReturn::Ok
            })
            .unwrap();
        let deadline = tilo_test_env::hang_deadline();
        let mut received = Vec::new();
        while received.len() < 3 {
            let polled_at = Instant::now();
            received.extend(
                audio
                    .poll_capture(polled_at)
                    .unwrap()
                    .into_iter()
                    .map(|frame| (polled_at, frame)),
            );
            assert!(
                Instant::now() < deadline,
                "jittered real Opus frames never reached capture"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(counter.load(std::sync::atomic::Ordering::Relaxed) >= 3);
        for pair in received.windows(2) {
            assert!(pair[1].1.timestamp_48k >= pair[0].1.timestamp_48k + 960);
        }
        for (polled_at, frame) in received {
            assert_eq!(frame.timestamp_48k % 960, 0);
            assert_eq!(frame.duration_samples, 960);
            assert!(polled_at.duration_since(frame.captured_at) < SEND_LIFETIME);
        }
        pad.remove_probe(probe);
        audio.close();
    }

    #[test]
    fn future_encoded_capture_waits_for_its_original_presentation_clock() {
        let _device_test = device_test_lock();
        let mut audio = software_output();
        let source = gst_app::AppSrc::builder()
            .is_live(true)
            .format(gst::Format::Time)
            .caps(&raw_caps())
            .build();
        let started = Instant::now();
        audio
            .start_capture_element(source.upcast_ref(), Arc::new(|| true))
            .unwrap();
        for frame in 0..3 {
            let mut buffer = gst::Buffer::from_slice(vec![0_u8; FRAME_SAMPLES * 2]);
            let writable = buffer.get_mut().unwrap();
            writable.set_pts(gst::ClockTime::from_mseconds(500 + frame * 20));
            writable.set_duration(gst::ClockTime::from_mseconds(20));
            source.push_buffer(buffer).unwrap();
        }
        assert!(audio.poll_capture(Instant::now()).unwrap().is_empty());
        assert!(
            !audio.capture_active(),
            "future data is not fresh capture readiness"
        );
        let deadline = tilo_test_env::hang_deadline();
        let (polled_at, frames) = loop {
            let polled_at = Instant::now();
            let frames = audio.poll_capture(polled_at).unwrap();
            if !frames.is_empty() {
                break (polled_at, frames);
            }
            assert!(
                Instant::now() < deadline,
                "original future packet never reached its clock"
            );
            std::thread::sleep(Duration::from_millis(5));
        };
        assert!(started.elapsed() >= Duration::from_millis(500));
        assert!(audio.capture_active());
        for frame in frames {
            assert_eq!(frame.timestamp_48k % 960, 0);
            assert_eq!(frame.duration_samples, 960);
            assert!(polled_at.duration_since(frame.captured_at) < SEND_LIFETIME);
        }
        audio.close();
    }

    #[test]
    fn loss_concealment_survives_async_decode_without_extending_receive_lifetime() {
        let _device_test = device_test_lock();
        let (mut audio, sink) = observed_output();
        let now = Instant::now();
        let id = PublicationId {
            namespace: "loss".into(),
            carrier_generation: 1,
            incarnation: 1,
        };
        audio
            .receive(
                ReceivedOpus {
                    publisher: id.clone(),
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
        let due = now + Duration::from_millis(100);
        tilo_test_env::wait_until("actual PLC PCM from the asynchronous decoder", || {
            let decoder = audio.decoders.get_mut(&id).unwrap();
            decoder.pull(due).unwrap();
            !decoder.ready.is_empty()
        });
        audio.playout(due).unwrap();
        assert_eq!(
            audio.last_output,
            Some(due),
            "PLC PCM must reach actual mixer after decoder scheduling delay"
        );
        // An expired nonzero PCM frame must never leak into the listening
        // silence, even when the decoder still has asynchronous work pending.
        audio
            .decoders
            .get_mut(&id)
            .unwrap()
            .ready
            .push_back(PcmFrame {
                samples: vec![i16::MAX; FRAME_SAMPLES],
                expires_at: now + RECEIVE_LIFETIME,
            });
        while sink.try_pull_sample(gst::ClockTime::ZERO).is_some() {}
        audio.playout(now + RECEIVE_LIFETIME).unwrap();
        assert!(
            audio.decoders[&id].ready.is_empty(),
            "PLC cannot outlive original receive window"
        );
        let sample = sink.try_pull_sample(hang_bound()).unwrap();
        assert_eq!(sample.buffer().unwrap().size(), FRAME_SAMPLES * 2);
        assert!(
            sample
                .buffer()
                .unwrap()
                .map_readable()
                .unwrap()
                .as_slice()
                .iter()
                .all(|byte| *byte == 0),
            "expired audio must be replaced by actual silent PCM"
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
            incarnation: 1,
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
    fn deafened_playout_plays_silence_and_consumes_the_call_unheard() {
        let _device_test = device_test_lock();
        let (mut audio, sink) = observed_output();
        let id = PublicationId {
            namespace: "deafened".into(),
            carrier_generation: 1,
            incarnation: 1,
        };
        // One frame of the call played at `at`, read back off the output.
        let played = |audio: &mut VoiceAudio, at: Instant| {
            let mut decoder = audio
                .decoders
                .remove(&id)
                .unwrap_or_else(|| Decoder::new().unwrap());
            decoder.ready.push_back(PcmFrame {
                samples: vec![1000; 960],
                expires_at: at + RECEIVE_LIFETIME,
            });
            audio.decoders.insert(id.clone(), decoder);
            audio.playout(at).unwrap();
            let sample = sink
                .try_pull_sample(hang_bound())
                .expect("the output carries a frame");
            let buffer = sample.buffer().unwrap().map_readable().unwrap();
            buffer
                .chunks_exact(2)
                .map(|pair| i16::from_le_bytes([pair[0], pair[1]]).unsigned_abs())
                .max()
                .unwrap_or(0)
        };
        let start = Instant::now();
        assert!(played(&mut audio, start) > 0, "the call is heard");
        audio.set_deafened(true);
        assert!(audio.deafened());
        assert_eq!(
            played(&mut audio, start + FRAME_DURATION),
            0,
            "deafened, the output is silent"
        );
        assert!(
            audio.decoders[&id].ready.len() <= 1,
            "the call plays on unheard, no backlog builds"
        );
        audio.set_deafened(false);
        assert!(
            played(&mut audio, start + FRAME_DURATION * 2) > 0,
            "undeafened, heard again"
        );
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
        assert!(encoder.property::<bool>("dtx"), "silence must not stream");
        assert!(!encoder.property::<bool>("inband-fec"));
    }

    /// The tilo-voice/1 object header each transmitted packet travels in.
    const OBJECT_HEADER: usize = 32;

    /// 48 kHz mono speech-band signal: a 140 Hz voice with formant-like
    /// harmonics and a 4 Hz syllable envelope that never falls silent.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "the signal peaks below 13000, which always fits i16"
    )]
    fn voiced(frames: usize) -> Vec<i16> {
        (0..frames.saturating_mul(FRAME_SAMPLES))
            .map(|index| {
                let t = f64::from(u32::try_from(index).unwrap()) / 48_000.0;
                let envelope = 0.55 + 0.45 * (t * 4.0 * std::f64::consts::TAU).sin();
                let voice: f64 = [(1, 1.0), (4, 0.6), (9, 0.35), (17, 0.2)]
                    .iter()
                    .map(|(harmonic, gain)| {
                        gain * (t * 140.0 * f64::from(*harmonic) * std::f64::consts::TAU).sin()
                    })
                    .sum();
                (voice * envelope * 6_000.0) as i16
            })
            .collect()
    }

    /// (transmitted packets, transmitted Opus bytes) among `packets`.
    fn transmitted(packets: &[Vec<u8>]) -> (usize, usize) {
        let sent: Vec<&Vec<u8>> = packets
            .iter()
            .filter(|packet| crate::voice::needs_transmission(packet))
            .collect();
        (sent.len(), sent.iter().map(|packet| packet.len()).sum())
    }

    #[test]
    fn a_silent_microphone_sends_a_few_comfort_noise_updates_per_second() {
        let _device_test = device_test_lock();
        let mut encoder = OpusEncoder::new().unwrap();
        // 4 s of silence; the encoder enters DTX after its ~200 ms hangover.
        let packets = encoder.encode(&vec![0; 4 * 48_000]);
        assert!(packets.len() >= 200, "{}", packets.len());
        let steady = &packets[50..200];
        // Over 3 s: at most 5 packets and 5 kbit/s of voice objects a second.
        let (sent, bytes) = transmitted(steady);
        let objects = bytes.saturating_add(sent.saturating_mul(OBJECT_HEADER));
        eprintln!("silent: {sent} packets and {objects} B of voice objects in 3 s");
        assert!(sent <= 15, "{sent} updates in 3 s");
        assert!(
            sent > 0,
            "comfort-noise updates keep the receiver's noise floor"
        );
        assert!(objects.saturating_mul(8) <= 15_000, "{objects} B in 3 s");
    }

    #[test]
    fn speech_is_sent_every_frame_from_its_onset_at_the_opus_bitrate() {
        let _device_test = device_test_lock();
        let mut encoder = OpusEncoder::new().unwrap();
        // 1 s of silence (deep in DTX), then 3 s of voice.
        let mut pcm = vec![0; 48_000];
        pcm.extend(voiced(150));
        let packets = encoder.encode(&pcm);
        assert!(packets.len() >= 200, "{}", packets.len());
        assert!(
            transmitted(&packets[25..50]).0 <= 3,
            "not in DTX before the onset"
        );
        // ...and the first voiced frame and every one after it is
        // transmitted: DTX never clips an onset.
        let speech = &packets[50..200];
        assert!(
            speech
                .iter()
                .all(|packet| crate::voice::needs_transmission(packet)),
            "{:?}",
            speech.iter().map(Vec::len).collect::<Vec<_>>()
        );
        let (sent, bytes) = transmitted(speech);
        let objects = bytes.saturating_add(sent.saturating_mul(OBJECT_HEADER));
        eprintln!("speaking: {sent} packets, {bytes} B Opus, {objects} B of voice objects in 3 s");
        assert_eq!(sent, 150);
        // Constrained VBR at 32 kbit/s, with a little room for the encoder's
        // short-term rate control.
        assert!(bytes.saturating_mul(8) <= 3 * 36_000, "{bytes} B in 3 s");
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
    fn delayed_polls_preserve_playback_rate_and_skip_long_outages() {
        let _device_test = device_test_lock();
        let (mut audio, sink) = observed_output();
        let base = Instant::now();
        // A 12ms service poll must keep the 20ms audio cadence, instead of
        // turning every poll delay into permanent sample-clock drift.
        let due = [0, 24, 48, 60, 84, 108, 120, 144, 168, 180, 204];
        for millis in (0..=204).step_by(12) {
            audio.playout(base + Duration::from_millis(millis)).unwrap();
            if due.contains(&millis) {
                let sample = sink
                    .try_pull_sample(hang_bound())
                    .unwrap_or_else(|| panic!("missing actual PCM at {millis}ms"));
                assert_eq!(sample.buffer().unwrap().size(), FRAME_SAMPLES * 2);
            } else {
                assert!(sink.try_pull_sample(gst::ClockTime::ZERO).is_none());
            }
        }
        // A long outage emits one current frame, with no catch-up burst.
        audio.playout(base + Duration::from_secs(1)).unwrap();
        assert!(sink.try_pull_sample(hang_bound()).is_some());
        audio.playout(base + Duration::from_millis(1001)).unwrap();
        assert!(sink.try_pull_sample(gst::ClockTime::ZERO).is_none());
        audio.close();
    }

    #[test]
    fn listening_without_publishers_clocks_bounded_silence_without_capture() {
        let _device_test = device_test_lock();
        let (mut audio, sink) = observed_output();
        let now = Instant::now();
        let deadline = tilo_test_env::hang_deadline();
        let sample = loop {
            audio.playout(now).unwrap();
            if let Some(sample) = sink.try_pull_sample(gst::ClockTime::ZERO) {
                break sample;
            }
            assert!(
                Instant::now() < deadline,
                "listening output produced no PCM"
            );
            std::thread::sleep(Duration::from_millis(1));
        };
        let buffer = sample.buffer().unwrap();
        assert_eq!(buffer.size(), FRAME_SAMPLES * 2);
        assert_eq!(buffer.duration(), Some(gst::ClockTime::from_mseconds(20)));
        assert!(
            buffer
                .map_readable()
                .unwrap()
                .as_slice()
                .iter()
                .all(|byte| *byte == 0)
        );
        assert!(!audio.capture_active());
        assert_eq!(audio.decoder_count(), 0);
        audio.playout(now + Duration::from_millis(19)).unwrap();
        assert!(sink.try_pull_sample(gst::ClockTime::ZERO).is_none());
        audio.playout(now + FRAME_DURATION).unwrap();
        assert!(sink.try_pull_sample(hang_bound()).is_some());
        audio.close();
        assert_eq!(
            audio.playout(now + FRAME_DURATION * 2),
            Err(AudioError::Closed)
        );
    }

    #[test]
    fn separate_publishers_decode_to_one_bounded_pcm_output_and_reset_on_reconnect() {
        let _device_test = device_test_lock();
        let (mut audio, sink) = observed_output();
        let now = Instant::now();
        let frame = |name: &str, generation| ReceivedOpus {
            publisher: PublicationId {
                namespace: name.into(),
                carrier_generation: generation,
                incarnation: 1,
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
            incarnation: 1,
        });
        assert_eq!(audio.decoder_count(), 0);
        audio.close();
        assert_eq!(
            audio.receive(frame("alice", 2), now),
            Err(AudioError::Closed)
        );
    }

    #[test]
    fn a_publication_plays_at_its_volume_clipped() {
        assert_eq!(super::louder(vec![1000, -1000, 0], 0), vec![0, 0, 0]);
        assert_eq!(super::louder(vec![1000, -1000], 50), vec![500, -500]);
        assert_eq!(
            super::louder(vec![i16::MAX, i16::MIN], 200),
            vec![i16::MAX, i16::MIN]
        );
    }
}
