//! Native `PipeWire` audio streams that report device timing.
//!
//! A playback stream pulls its audio from a [`PlaybackSource`] inside the
//! stream's process callback, handing it the local `CLOCK_MONOTONIC` time at
//! which the first sample of the block will leave the speaker. That time comes
//! from `pw_stream_get_time`: the cycle's timestamp plus the delay to the
//! device plus whatever is still queued in the stream, so a
//! [`SyncPlayout`] behind it can schedule
//! against the moment the listener hears a sample rather than the moment it
//! was written.
//!
//! A capture stream records one chosen node, named by its `object.serial`.
//! Only that node: the stream asks the session manager never to fall back to
//! the default source (which may be the microphone), and it watches the graph
//! to confirm that the only link into it comes from the chosen node. Audio is
//! delivered once that is confirmed; any other link fails the stream.
//!
//! Each stream runs its own `PipeWire` loop on a dedicated thread, never on the
//! compositor's. Opening a stream goes through the [`AudioStreams`] trait, so
//! the code that decides what to play or record is tested against a fake
//! backend without a `PipeWire` daemon; the rendering and verification logic
//! here is pure and tested the same way.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::sync_playout::{ChannelRole, ClockMapping, SyncPlayout};
use tilo_lock::MutexRecover as _;

/// Nanoseconds per second.
const NS_PER_SECOND: i64 = 1_000_000_000;

/// How far a capture cycle's measured start may stray from where the
/// previous cycle ended and still count as continuous. Cycle timestamps
/// jitter by a few microseconds; a real discontinuity (an xrun, a suspended
/// device) is a whole quantum or more.
pub const CAPTURE_CONTINUITY_TOLERANCE_NS: i64 = 2_000_000;

/// Bytes per sample of the playback stream's `F32LE` format.
const F32_BYTES: usize = 4;

/// Bytes per sample of the capture stream's `S16LE` format.
pub const S16_BYTES: usize = 2;

/// This device's `CLOCK_MONOTONIC` in nanoseconds: the clock `PipeWire`
/// timestamps its graph cycles with.
#[cfg(unix)]
#[allow(
    clippy::useless_conversion,
    reason = "time_t and c_long are 32 bits wide on 32-bit targets"
)]
#[cfg(unix)]
pub fn monotonic_now_ns() -> i64 {
    let mut time = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `time` is a valid, writable timespec for the duration of the
    // call, and CLOCK_MONOTONIC is always available on Linux.
    let status = unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &raw mut time) };
    if status != 0 {
        return 0;
    }
    i64::from(time.tv_sec)
        .saturating_mul(NS_PER_SECOND)
        .saturating_add(i64::from(time.tv_nsec))
}

/// Hosts without a `CLOCK_MONOTONIC` report the clock as unreadable, as a
/// failed `clock_gettime` does.
#[cfg(not(unix))]
pub fn monotonic_now_ns() -> i64 {
    0
}

pub fn frames_to_ns(frames: i64, rate: u32) -> i64 {
    let ns = i128::from(frames)
        .saturating_mul(i128::from(NS_PER_SECOND))
        .checked_div(i128::from(rate))
        .unwrap_or(0);
    saturate_i128(ns)
}

/// `ns` as whole frames at `rate`, rounded towards negative infinity so that
/// consecutive instants map to non-decreasing frame indices.
pub fn ns_to_frames(ns: i64, rate: u32) -> i64 {
    let scaled = i128::from(ns).saturating_mul(i128::from(rate));
    let frames = scaled
        .checked_div_euclid(i128::from(NS_PER_SECOND))
        .unwrap_or(0);
    saturate_i128(frames)
}

fn saturate_i128(value: i128) -> i64 {
    i64::try_from(value).unwrap_or(if value < 0 { i64::MIN } else { i64::MAX })
}

/// One `pw_time` report, converted to nanoseconds and stream frames.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DeviceTiming {
    /// `CLOCK_MONOTONIC` nanoseconds at the start of the graph cycle this
    /// report describes.
    pub now_ns: i64,
    /// Time between the stream and the device, including every filter on the
    /// path. May be negative.
    pub delay_ns: i64,
    /// Frames the stream holds that the graph has not consumed yet: queued
    /// buffers plus the resampler's history.
    pub queued_frames: u64,
}

impl DeviceTiming {
    /// Builds a report from `pw_time`'s raw fields. `delay` counts ticks of
    /// the graph rate `rate_num / rate_denom` seconds; `queued_bytes` and
    /// `buffered_frames` count the stream's own format, `stride` bytes per
    /// frame.
    pub fn from_raw(
        now_ns: i64,
        rate_num: u32,
        rate_denom: u32,
        delay: i64,
        queued_bytes: u64,
        buffered_frames: u64,
        stride: usize,
    ) -> Self {
        let delay_ns = i128::from(delay)
            .saturating_mul(i128::from(rate_num))
            .saturating_mul(i128::from(NS_PER_SECOND))
            .checked_div(i128::from(rate_denom))
            .unwrap_or(0);
        let queued = u64::try_from(stride)
            .ok()
            .and_then(|stride| queued_bytes.checked_div(stride))
            .unwrap_or(0);
        Self {
            now_ns,
            delay_ns: saturate_i128(delay_ns),
            queued_frames: queued.saturating_add(buffered_frames),
        }
    }

    /// Whether the report describes a running graph. Before the first cycle
    /// `pw_time` is all zeros.
    pub const fn is_live(&self) -> bool {
        self.now_ns > 0
    }

    /// Local time at which the next frame written to a playback stream leaves
    /// the speaker.
    pub fn playback_presentation_ns(&self, rate: u32) -> i64 {
        let queued = frames_to_ns(i64::try_from(self.queued_frames).unwrap_or(i64::MAX), rate);
        self.now_ns
            .saturating_add(self.delay_ns)
            .saturating_add(queued)
    }

    /// Local time at which the first of the `frames` frames a capture stream
    /// delivers this cycle entered the device: the buffer ends where the
    /// device delay before the cycle start puts it, and spans its own
    /// duration before that.
    pub fn capture_origin_ns(&self, frames: usize, rate: u32) -> i64 {
        let span = frames_to_ns(i64::try_from(frames).unwrap_or(i64::MAX), rate);
        self.now_ns
            .saturating_sub(self.delay_ns)
            .saturating_sub(span)
    }
}

/// Where a stream's timing comes from: the live stream in production, a
/// scripted clock in tests.
pub trait StreamClock {
    /// The stream's current timing report, if it has one.
    fn timing(&self) -> Option<DeviceTiming>;
    /// The local monotonic time now.
    fn now_ns(&self) -> i64;
}

/// Audio for a playback stream, pulled from its process callback.
pub trait PlaybackSource: Send + 'static {
    /// Fills `out` (interleaved `f32`, the stream's channel count per frame)
    /// whose first frame leaves the speaker at local monotonic time
    /// `presentation_ns`. Always fills the whole buffer.
    fn render(&mut self, presentation_ns: i64, out: &mut [f32]);
}

/// A [`SyncPlayout`] and the clock mapping it renders against, shared between
/// the thread that receives frames and the stream that plays them.
#[derive(Debug)]
pub struct SyncPlayoutState<C> {
    pub playout: SyncPlayout,
    pub clock: C,
}

/// Plays a shared [`SyncPlayout`]: the receiving side pushes frames and
/// updates the clock through the same `Arc`.
pub struct SyncPlaybackSource<C> {
    shared: Arc<Mutex<SyncPlayoutState<C>>>,
}

impl<C> SyncPlaybackSource<C> {
    pub const fn new(shared: Arc<Mutex<SyncPlayoutState<C>>>) -> Self {
        Self { shared }
    }
}

impl<C: ClockMapping + Send + 'static> PlaybackSource for SyncPlaybackSource<C> {
    fn render(&mut self, presentation_ns: i64, out: &mut [f32]) {
        let mut state = self.shared.lock_recover();
        let SyncPlayoutState { playout, clock } = &mut *state;
        playout.render(clock, presentation_ns, out);
        drop(state);
    }
}

/// Where the last timing report of a stream is kept for its owner to read.
#[derive(Debug, Default)]
pub struct StreamStatus {
    stop: AtomicBool,
    timing: Mutex<Option<DeviceTiming>>,
    failure: Mutex<Option<String>>,
}

impl StreamStatus {
    pub fn timing(&self) -> Option<DeviceTiming> {
        *self.timing.lock_recover()
    }

    pub fn failure(&self) -> Option<String> {
        self.failure.lock_recover().clone()
    }

    /// Records the first failure; later ones are consequences of it.
    pub fn fail(&self, reason: impl Into<String>) {
        let mut failure = self.failure.lock_recover();
        if failure.is_none() {
            *failure = Some(reason.into());
        }
    }

    pub fn request_stop(&self) {
        self.stop.store(true, Ordering::Release);
    }

    pub fn stop_requested(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }

    fn record_timing(&self, timing: DeviceTiming) {
        *self.timing.lock_recover() = Some(timing);
    }
}

/// The body of a playback stream's process callback: asks the source for one
/// block at the right presentation time and writes it as `F32LE`.
pub struct PlaybackRenderer {
    source: Box<dyn PlaybackSource>,
    channels: usize,
    rate: u32,
    scratch: Vec<f32>,
    status: Arc<StreamStatus>,
}

impl PlaybackRenderer {
    pub fn new(
        source: Box<dyn PlaybackSource>,
        channels: u16,
        rate: u32,
        status: Arc<StreamStatus>,
    ) -> Self {
        Self {
            source,
            channels: usize::from(channels.max(1)),
            rate: rate.max(1),
            scratch: Vec::new(),
            status,
        }
    }

    /// Bytes per interleaved frame.
    pub const fn stride(&self) -> usize {
        self.channels.saturating_mul(F32_BYTES)
    }

    /// Fills `out` with at most `requested_frames` frames (all that fit when
    /// the graph did not say) and returns the bytes written. The block is
    /// scheduled at the device's presentation time; before the stream reports
    /// timing it is scheduled for now.
    pub fn fill(
        &mut self,
        clock: &dyn StreamClock,
        out: &mut [u8],
        requested_frames: Option<usize>,
    ) -> usize {
        let stride = self.stride();
        let capacity = out.len().checked_div(stride).unwrap_or(0);
        let frames = requested_frames
            .filter(|&requested| requested > 0)
            .map_or(capacity, |requested| requested.min(capacity));
        if frames == 0 {
            return 0;
        }
        let timing = clock.timing().filter(DeviceTiming::is_live);
        let presentation_ns = match timing {
            Some(timing) => {
                self.status.record_timing(timing);
                timing.playback_presentation_ns(self.rate)
            }
            None => clock.now_ns(),
        };
        let samples = frames.saturating_mul(self.channels);
        self.scratch.clear();
        self.scratch.resize(samples, 0.0);
        self.source.render(presentation_ns, &mut self.scratch);
        for (bytes, sample) in out.chunks_exact_mut(F32_BYTES).zip(&self.scratch) {
            bytes.copy_from_slice(&sample.clamp(-1.0, 1.0).to_le_bytes());
        }
        frames.saturating_mul(stride)
    }
}

/// How a playback stream is opened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlaybackConfig {
    /// `node.name`: stable, so session policy can match it.
    pub node_name: String,
    /// `node.description`: what a volume mixer shows.
    pub description: String,
    pub rate: u32,
    /// Interleaved channels the source renders: 1 is played as mono, 2 as
    /// front left and right.
    pub channels: u16,
    /// Graph quantum the stream asks for, in frames at `rate`.
    pub latency_frames: u32,
    /// Play on the sink with this `object.serial` and nowhere else; `None`
    /// follows the default output.
    pub target_serial: Option<u32>,
}

impl PlaybackConfig {
    /// A stream carrying `role`'s channels at `rate` with a 10 ms quantum.
    pub fn for_role(node_name: &str, description: &str, rate: u32, role: ChannelRole) -> Self {
        Self {
            node_name: node_name.into(),
            description: description.into(),
            rate,
            channels: u16::try_from(role.channels()).unwrap_or(2),
            latency_frames: rate.checked_div(100).unwrap_or(480).max(1),
            target_serial: None,
        }
    }

    /// Stream properties, as `PipeWire` key/value pairs.
    pub fn properties(&self) -> Vec<(&'static str, String)> {
        let mut properties = vec![
            ("media.type", "Audio".into()),
            ("media.category", "Playback".into()),
            ("media.role", "Music".into()),
            ("node.name", sanitize(&self.node_name)),
            ("node.description", sanitize(&self.description)),
            ("audio.channels", self.channels.to_string()),
            ("audio.rate", self.rate.to_string()),
            (
                "node.latency",
                format!("{}/{}", self.latency_frames, self.rate),
            ),
        ];
        if let Some(serial) = self.target_serial {
            properties.extend([
                ("target.object", serial.to_string()),
                ("node.dont-fallback", "true".into()),
                ("node.dont-reconnect", "true".into()),
                ("node.dont-move", "true".into()),
            ]);
        }
        properties
    }
}

/// The node a capture stream records.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CaptureTarget {
    /// The node's `object.serial`.
    pub serial: u32,
    /// Record what a sink plays (its monitor) rather than a source's output.
    pub monitor_sink: bool,
}

/// How a capture stream is opened. The format is always `S16LE`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptureConfig {
    pub node_name: String,
    pub target: CaptureTarget,
    pub rate: u32,
    pub channels: u16,
    /// Chunks buffered for [`CaptureStream::drain`]; beyond it the newest
    /// chunk is dropped, so a stalled reader loses audio instead of growing
    /// latency.
    pub queue_chunks: usize,
}

impl CaptureConfig {
    /// Stream properties, as `PipeWire` key/value pairs. The target is named
    /// by serial, and every way the session manager could substitute another
    /// node is switched off: no fallback to a default when the target is
    /// missing, no reconnect elsewhere when it goes away, no moving it later.
    pub fn properties(&self) -> Vec<(&'static str, String)> {
        let mut properties = vec![
            ("media.type", "Audio".into()),
            ("media.category", "Capture".into()),
            ("node.name", sanitize(&self.node_name)),
            ("target.object", self.target.serial.to_string()),
            ("node.dont-fallback", "true".into()),
            ("node.dont-reconnect", "true".into()),
            ("node.dont-move", "true".into()),
            ("audio.channels", self.channels.to_string()),
            ("audio.rate", self.rate.to_string()),
        ];
        if self.target.monitor_sink {
            properties.push(("stream.capture.sink", "true".into()));
        }
        properties
    }

    /// Bytes per interleaved frame.
    pub fn stride(&self) -> usize {
        usize::from(self.channels.max(1)).saturating_mul(S16_BYTES)
    }
}

/// `PipeWire` property values are C strings.
fn sanitize(value: &str) -> String {
    value
        .chars()
        .filter(|&character| character != '\0')
        .collect()
}

/// A run of captured audio.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptureChunk {
    /// Index of the chunk's first frame on this device's monotonic clock,
    /// counted in frames at the stream rate: `ns · rate / 10⁹`. Consecutive
    /// chunks of an uninterrupted capture continue exactly where the previous
    /// one ended; a gap in the indices is audio the device never delivered.
    pub first_frame: i64,
    /// Interleaved `S16LE`, whole frames only.
    pub pcm_s16le: Vec<u8>,
}

/// Stamps capture cycles with frame indices on the monotonic clock, keeping
/// an uninterrupted capture sample-continuous despite timestamp jitter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CaptureTimeline {
    rate: u32,
    next_frame: Option<i64>,
}

impl CaptureTimeline {
    pub const fn new(rate: u32) -> Self {
        Self {
            rate,
            next_frame: None,
        }
    }

    /// The index of the first of `frames` frames whose first one the device
    /// captured at `origin_ns`.
    pub fn stamp(&mut self, origin_ns: i64, frames: usize) -> i64 {
        let measured = ns_to_frames(origin_ns, self.rate);
        let tolerance = ns_to_frames(CAPTURE_CONTINUITY_TOLERANCE_NS, self.rate);
        let first = match self.next_frame {
            Some(expected) if measured.abs_diff(expected) <= tolerance.unsigned_abs() => expected,
            _ => measured,
        };
        self.next_frame = Some(first.saturating_add(i64::try_from(frames).unwrap_or(i64::MAX)));
        first
    }
}

/// What the graph says about a capture stream's input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkVerdict {
    /// Nothing is linked into the stream yet.
    Waiting,
    /// Only the chosen node is linked into the stream.
    Linked,
    /// Another node is linked into the stream.
    Foreign { node: u32 },
    /// The chosen node left the graph.
    TargetGone,
}

/// Confirms from the registry that a capture stream records exactly the
/// chosen node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TargetLinkGuard {
    target_serial: u32,
    target_node: Option<u32>,
    own_node: Option<u32>,
    /// `(output node, input node)` of every link seen.
    links: Vec<(u32, u32)>,
    target_gone: bool,
}

impl TargetLinkGuard {
    pub const fn new(target_serial: u32) -> Self {
        Self {
            target_serial,
            target_node: None,
            own_node: None,
            links: Vec::new(),
            target_gone: false,
        }
    }

    /// A node appeared in the registry.
    pub fn observe_node(&mut self, id: u32, serial: Option<u32>) {
        if serial == Some(self.target_serial) {
            self.target_node = Some(id);
            self.target_gone = false;
        }
    }

    /// A global left the registry.
    pub fn observe_removal(&mut self, id: u32) {
        if self.target_node == Some(id) {
            self.target_gone = true;
        }
        self.links
            .retain(|&(output, input)| output != id && input != id);
    }

    /// A link appeared in the registry.
    pub fn observe_link(&mut self, output_node: u32, input_node: u32) {
        self.links.push((output_node, input_node));
    }

    /// A known link left the registry. Node ids alone cannot identify link removal.
    pub fn remove_link(&mut self, output_node: u32, input_node: u32) {
        self.links
            .retain(|&(output, input)| output != output_node || input != input_node);
    }

    /// The stream's own node id, once `PipeWire` assigned it.
    pub const fn set_own_node(&mut self, id: u32) {
        self.own_node = Some(id);
    }

    pub fn verdict(&self) -> LinkVerdict {
        if self.target_gone {
            return LinkVerdict::TargetGone;
        }
        let Some(own) = self.own_node else {
            return LinkVerdict::Waiting;
        };
        let mut linked = false;
        for &(output, input) in &self.links {
            if input != own {
                continue;
            }
            if Some(output) == self.target_node {
                linked = true;
            } else if self.target_node.is_some() || output != own {
                return LinkVerdict::Foreign { node: output };
            }
        }
        if linked {
            LinkVerdict::Linked
        } else {
            LinkVerdict::Waiting
        }
    }
}

pub fn whole_frames(bytes: &[u8], stride: usize) -> &[u8] {
    let whole = bytes
        .len()
        .checked_div(stride)
        .unwrap_or(0)
        .saturating_mul(stride);
    bytes.get(..whole).unwrap_or_default()
}

/// Plays one buffer so that its first frame leaves the speaker at a chosen
/// local time: the playback half of a timed measurement such as an acoustic
/// sweep. Frames scheduled before the stream could play them are reported
/// through [`ScheduleProgress::late_by_ns`] rather than silently shifted.
#[derive(Debug)]
pub struct ScheduledBuffer {
    samples: Arc<[f32]>,
    channels: usize,
    rate: u32,
    start_ns: i64,
    progress: Arc<ScheduleProgress>,
}

/// What a [`ScheduledBuffer`] has played so far, readable from any thread.
#[derive(Debug, Default)]
pub struct ScheduleProgress {
    /// How late the first frame was presented, once known; 0 when on time.
    late_by_ns: Mutex<Option<i64>>,
    /// Whether the last frame has been handed to the device.
    finished: AtomicBool,
}

impl ScheduleProgress {
    pub fn late_by_ns(&self) -> Option<i64> {
        *self.late_by_ns.lock_recover()
    }

    pub fn finished(&self) -> bool {
        self.finished.load(Ordering::Acquire)
    }
}

impl ScheduledBuffer {
    /// `samples` is interleaved `f32` with `channels` per frame.
    pub fn new(samples: Vec<f32>, channels: u16, rate: u32, start_ns: i64) -> Self {
        Self {
            samples: samples.into(),
            channels: usize::from(channels.max(1)),
            rate: rate.max(1),
            start_ns,
            progress: Arc::default(),
        }
    }

    pub fn progress(&self) -> Arc<ScheduleProgress> {
        Arc::clone(&self.progress)
    }

    fn frames(&self) -> i64 {
        i64::try_from(self.samples.len().checked_div(self.channels).unwrap_or(0))
            .unwrap_or(i64::MAX)
    }
}

impl PlaybackSource for ScheduledBuffer {
    fn render(&mut self, presentation_ns: i64, out: &mut [f32]) {
        out.fill(0.0);
        // The buffer frame presented first, rounded to the nearest frame.
        let half_frame = frames_to_ns(1, self.rate) / 2;
        let first = ns_to_frames(
            presentation_ns
                .saturating_sub(self.start_ns)
                .saturating_add(half_frame),
            self.rate,
        );
        let count =
            i64::try_from(out.len().checked_div(self.channels).unwrap_or(0)).unwrap_or(i64::MAX);
        let end = first.saturating_add(count);
        {
            let mut late = self.progress.late_by_ns.lock_recover();
            if late.is_none() && end > 0 {
                *late = Some(frames_to_ns(first.max(0), self.rate));
            }
        }
        let from = first.max(0);
        let to = end.min(self.frames());
        if to >= self.frames() {
            self.progress.finished.store(true, Ordering::Release);
        }
        if from >= to {
            return;
        }
        let skip = usize::try_from(from.saturating_sub(first)).unwrap_or(0);
        let source = usize::try_from(from).unwrap_or(0);
        let frames = usize::try_from(to.saturating_sub(from)).unwrap_or(0);
        let src = self
            .samples
            .get(source.saturating_mul(self.channels)..)
            .unwrap_or_default();
        let dst = out
            .get_mut(skip.saturating_mul(self.channels)..)
            .unwrap_or_default();
        for (target, sample) in dst
            .iter_mut()
            .zip(src)
            .take(frames.saturating_mul(self.channels))
        {
            *target = *sample;
        }
    }
}

/// Samples recorded over a time window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapturedWindow {
    /// Local `CLOCK_MONOTONIC` nanoseconds at which the first frame entered
    /// the device.
    pub first_sample_ns: i64,
    pub rate: u32,
    pub channels: u16,
    /// Interleaved; audio the device never delivered is zeros.
    pub samples: Vec<i16>,
}

/// Cuts `[start_ns, start_ns + duration_ns)` out of captured chunks. The
/// window starts at the first captured frame at or after `start_ns`; gaps are
/// filled with silence. `None` when no chunk reaches into the window.
pub fn assemble_window(
    chunks: &[CaptureChunk],
    rate: u32,
    channels: u16,
    start_ns: i64,
    duration_ns: i64,
) -> Option<CapturedWindow> {
    let width = usize::from(channels.max(1));
    let stride = width.saturating_mul(S16_BYTES);
    // First frame whose capture time is at or after `start_ns`.
    let wanted = ns_to_frames(start_ns.saturating_sub(1), rate).saturating_add(1);
    let end = ns_to_frames(start_ns.saturating_add(duration_ns).saturating_sub(1), rate)
        .saturating_add(1);
    let available = chunks
        .iter()
        .filter(|chunk| !chunk.pcm_s16le.is_empty())
        .map(|chunk| chunk.first_frame)
        .min()?;
    let first = wanted.max(available);
    if first >= end {
        return None;
    }
    let frames = usize::try_from(end.saturating_sub(first)).ok()?;
    let mut samples = vec![0_i16; frames.saturating_mul(width)];
    for chunk in chunks {
        let pcm = whole_frames(&chunk.pcm_s16le, stride);
        for (index, frame) in pcm.chunks_exact(stride).enumerate() {
            let at = chunk
                .first_frame
                .saturating_add(i64::try_from(index).unwrap_or(i64::MAX));
            if at < first || at >= end {
                continue;
            }
            let offset = usize::try_from(at.saturating_sub(first))
                .unwrap_or(usize::MAX)
                .saturating_mul(width);
            let slots = samples.get_mut(offset..).unwrap_or_default();
            for (slot, bytes) in slots.iter_mut().zip(frame.chunks_exact(S16_BYTES)) {
                *slot = <[u8; 2]>::try_from(bytes).map_or(0, i16::from_le_bytes);
            }
        }
    }
    Some(CapturedWindow {
        first_sample_ns: frames_to_ns(first, rate),
        rate,
        channels,
        samples,
    })
}

/// Sample-accurate timed playback and capture on chosen local nodes, timed by
/// `PipeWire`'s device clock rather than a subprocess's start-up: the
/// primitives acoustic calibration measures with. Both calls block until
/// their window has passed, so run them off the compositor thread (and on two
/// threads to play and record at once).
pub struct TimedAudio<'a> {
    streams: &'a dyn AudioStreams,
}

/// How often a timed call checks on its stream.
const TIMED_POLL: std::time::Duration = std::time::Duration::from_millis(5);
/// How long past its window a timed call waits for the device before failing.
const TIMED_GRACE_NS: i64 = 2_000_000_000;

impl TimedAudio<'static> {
    /// Timed audio on this device's `PipeWire` daemon.
    pub fn native() -> Self {
        Self {
            streams: native_streams(),
        }
    }
}

impl<'a> TimedAudio<'a> {
    pub const fn new(streams: &'a dyn AudioStreams) -> Self {
        Self { streams }
    }

    /// Plays `samples` (interleaved `f32`, `channels` per frame, at `rate`)
    /// on the sink with `object.serial` `sink_serial`, so that the first
    /// frame leaves the speaker at local `CLOCK_MONOTONIC` time `start_ns`.
    /// Returns once the last frame has been handed to the device.
    ///
    /// # Errors
    ///
    /// Fails when the stream cannot start or stops, or [`StreamError::Late`]
    /// when the device could not present the first frame on time (start the
    /// call earlier: opening a stream takes tens of milliseconds).
    pub fn play_at(
        &self,
        sink_serial: u32,
        rate: u32,
        channels: u16,
        samples: Vec<f32>,
        start_ns: i64,
    ) -> Result<(), StreamError> {
        let buffer = ScheduledBuffer::new(samples, channels, rate, start_ns);
        let progress = buffer.progress();
        let deadline = start_ns
            .saturating_add(frames_to_ns(buffer.frames(), rate))
            .saturating_add(TIMED_GRACE_NS);
        let config = PlaybackConfig {
            node_name: "tilo-timed-playback".into(),
            description: "Tilo measurement".into(),
            rate,
            channels,
            latency_frames: rate.checked_div(200).unwrap_or(240).max(1),
            target_serial: Some(sink_serial),
        };
        let stream = self.streams.open_playback(&config, Box::new(buffer))?;
        loop {
            if let Some(failure) = stream.failure() {
                return Err(StreamError::Setup(failure));
            }
            if let Some(late) = progress.late_by_ns().filter(|late| *late > 0) {
                return Err(StreamError::Late { by_ns: late });
            }
            if progress.finished() {
                return Ok(());
            }
            if monotonic_now_ns() > deadline {
                return Err(StreamError::Setup(
                    "the device did not play the buffer".into(),
                ));
            }
            std::thread::sleep(TIMED_POLL);
        }
    }

    /// Records `duration_ns` from `target` starting at local
    /// `CLOCK_MONOTONIC` time `start_ns`. The window starts at the first
    /// frame captured at or after `start_ns`, whose device time is reported.
    ///
    /// # Errors
    ///
    /// Fails when the stream cannot start, stops, is linked to anything but
    /// `target`, or delivers nothing inside the window.
    pub fn capture_window(
        &self,
        target: CaptureTarget,
        rate: u32,
        channels: u16,
        start_ns: i64,
        duration_ns: i64,
    ) -> Result<CapturedWindow, StreamError> {
        let config = CaptureConfig {
            node_name: "tilo-timed-capture".into(),
            target,
            rate,
            channels,
            queue_chunks: 4096,
        };
        let end_ns = start_ns.saturating_add(duration_ns);
        let end_frame = ns_to_frames(end_ns, rate);
        let mut stream = self.streams.open_capture(&config)?;
        let mut chunks: Vec<CaptureChunk> = Vec::new();
        loop {
            let drained = stream.drain().map_err(StreamError::Setup)?;
            // Keep only what can still reach into the window.
            chunks.extend(drained.into_iter().filter(|chunk| {
                let frames = chunk
                    .pcm_s16le
                    .len()
                    .checked_div(config.stride())
                    .unwrap_or(0);
                chunk
                    .first_frame
                    .saturating_add(i64::try_from(frames).unwrap_or(i64::MAX))
                    > ns_to_frames(start_ns, rate)
            }));
            let covered = chunks.iter().any(|chunk| {
                let frames = chunk
                    .pcm_s16le
                    .len()
                    .checked_div(config.stride())
                    .unwrap_or(0);
                chunk
                    .first_frame
                    .saturating_add(i64::try_from(frames).unwrap_or(i64::MAX))
                    >= end_frame
            });
            if covered || monotonic_now_ns() > end_ns.saturating_add(TIMED_GRACE_NS) {
                break;
            }
            std::thread::sleep(TIMED_POLL);
        }
        assemble_window(&chunks, rate, channels, start_ns, duration_ns).ok_or_else(|| {
            StreamError::Setup("the device delivered nothing inside the window".into())
        })
    }
}

/// Why a stream could not be opened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StreamError {
    /// No `PipeWire` daemon to talk to, or no `PipeWire` support in this build.
    Unavailable(String),
    /// The daemon refused the stream.
    Setup(String),
    /// A scheduled buffer's first frame reached the speaker this late.
    Late { by_ns: i64 },
}

impl std::fmt::Display for StreamError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable(reason) => write!(formatter, "PipeWire is unavailable: {reason}"),
            Self::Setup(reason) => write!(formatter, "PipeWire stream setup failed: {reason}"),
            Self::Late { by_ns } => write!(
                formatter,
                "the device presented the buffer {by_ns} ns after its start time"
            ),
        }
    }
}

impl std::error::Error for StreamError {}

/// A running playback stream. Dropping it stops the stream.
pub trait PlaybackStream: Send {
    /// The latest device timing the stream reported.
    fn timing(&self) -> Option<DeviceTiming>;
    /// Why the stream stopped, once it has.
    fn failure(&self) -> Option<String>;
}

/// One-shot custody of an output verified by the local device observer.
/// This descriptor stays in the host; apps never receive it.
#[cfg(unix)]
pub struct ObservedPlayback {
    pub socket: std::os::fd::OwnedFd,
    pub cookie: u32,
    pub serial: u32,
}

/// Open an output on the observer's exact connection, pinned to its sink.
#[cfg(unix)]
pub fn open_observed_playback(
    config: &PlaybackConfig,
    source: Box<dyn PlaybackSource>,
    observed: ObservedPlayback,
) -> Result<Box<dyn PlaybackStream>, StreamError> {
    #[cfg(all(feature = "pipewire", target_os = "linux"))]
    {
        native::observed_playback(config, source, observed)
    }
    #[cfg(not(all(feature = "pipewire", target_os = "linux")))]
    {
        let _ = (config, source, observed);
        Err(StreamError::Unavailable(
            "built without native PipeWire output".into(),
        ))
    }
}

/// A running capture stream. Dropping it stops the stream.
pub trait CaptureStream: Send {
    /// Everything captured since the last call.
    ///
    /// # Errors
    ///
    /// Returns why the stream stopped: the target went away, another node was
    /// linked into it, or the daemon ended it.
    fn drain(&mut self) -> Result<Vec<CaptureChunk>, String>;
}

/// Opens audio streams. [`native_streams`] is the real one.
pub trait AudioStreams: Send + Sync {
    /// Starts a stream that pulls from `source`.
    fn open_playback(
        &self,
        config: &PlaybackConfig,
        source: Box<dyn PlaybackSource>,
    ) -> Result<Box<dyn PlaybackStream>, StreamError>;

    /// Starts recording `config.target`.
    fn open_capture(&self, config: &CaptureConfig) -> Result<Box<dyn CaptureStream>, StreamError>;
}

/// The `PipeWire` stream backend of this build.
pub fn native_streams() -> &'static dyn AudioStreams {
    #[cfg(all(feature = "pipewire", target_os = "linux"))]
    {
        &native::PipeWireStreams
    }
    #[cfg(not(all(feature = "pipewire", target_os = "linux")))]
    {
        &Unsupported
    }
}

#[cfg(not(all(feature = "pipewire", target_os = "linux")))]
struct Unsupported;

#[cfg(not(all(feature = "pipewire", target_os = "linux")))]
impl AudioStreams for Unsupported {
    fn open_playback(
        &self,
        _config: &PlaybackConfig,
        _source: Box<dyn PlaybackSource>,
    ) -> Result<Box<dyn PlaybackStream>, StreamError> {
        Err(StreamError::Unavailable(
            "built without PipeWire stream support".into(),
        ))
    }

    fn open_capture(&self, _config: &CaptureConfig) -> Result<Box<dyn CaptureStream>, StreamError> {
        Err(StreamError::Unavailable(
            "built without PipeWire stream support".into(),
        ))
    }
}

#[cfg(all(feature = "pipewire", target_os = "linux"))]
mod native {
    //! The `PipeWire` loop behind [`AudioStreams`](super::AudioStreams).

    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, sync_channel};
    use std::sync::{Arc, Once};
    use std::thread::JoinHandle;
    use std::time::{Duration, Instant};

    use pipewire as pw;
    use pw::spa;
    use pw::spa::pod::Pod;

    use super::{
        AudioStreams, CaptureChunk, CaptureConfig, CaptureStream, CaptureTimeline, DeviceTiming,
        LinkVerdict, PlaybackConfig, PlaybackRenderer, PlaybackSource, PlaybackStream, StreamClock,
        StreamError, StreamStatus, TargetLinkGuard, frames_to_ns, monotonic_now_ns, whole_frames,
    };

    /// How long opening a stream may take before it is reported as failed.
    const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
    /// How long a capture stream may stay unlinked before the chosen node is
    /// declared unavailable.
    const LINK_TIMEOUT: Duration = Duration::from_secs(3);
    /// Loop wake-up interval, bounding how late a stop request is noticed.
    const LOOP_TICK: Duration = Duration::from_millis(20);

    pub(super) struct PipeWireStreams;

    fn ensure_initialised() {
        static INIT: Once = Once::new();
        INIT.call_once(pw::init);
    }

    struct LiveClock<'a> {
        stream: &'a pw::stream::Stream,
        stride: usize,
    }

    impl StreamClock for LiveClock<'_> {
        fn timing(&self) -> Option<DeviceTiming> {
            let time = self.stream.time().ok()?;
            let rate = time.rate();
            Some(DeviceTiming::from_raw(
                time.now(),
                rate.num,
                rate.denom,
                time.delay(),
                time.queued(),
                time.buffered(),
                self.stride,
            ))
        }

        fn now_ns(&self) -> i64 {
            monotonic_now_ns()
        }
    }

    fn properties(pairs: Vec<(&'static str, String)>) -> pw::properties::PropertiesBox {
        let mut properties = pw::properties::PropertiesBox::new();
        for (key, value) in pairs {
            properties.insert(key, value);
        }
        properties
    }

    fn format_pod(
        format: spa::param::audio::AudioFormat,
        rate: u32,
        channels: u16,
    ) -> Result<Vec<u8>, StreamError> {
        let mut info = spa::param::audio::AudioInfoRaw::new();
        info.set_format(format);
        info.set_rate(rate);
        info.set_channels(u32::from(channels));
        let mut position = [0; spa::sys::SPA_AUDIO_MAX_CHANNELS as usize];
        let map: &[u32] = if channels == 1 {
            &[spa::sys::SPA_AUDIO_CHANNEL_MONO]
        } else {
            &[
                spa::sys::SPA_AUDIO_CHANNEL_FL,
                spa::sys::SPA_AUDIO_CHANNEL_FR,
            ]
        };
        for (slot, channel) in position.iter_mut().zip(map) {
            *slot = *channel;
        }
        info.set_position(position);
        spa::pod::serialize::PodSerializer::serialize(
            std::io::Cursor::new(Vec::new()),
            &spa::pod::Value::Object(spa::pod::Object {
                type_: spa::sys::SPA_TYPE_OBJECT_Format,
                id: spa::sys::SPA_PARAM_EnumFormat,
                properties: info.into(),
            }),
        )
        .map(|(cursor, _)| cursor.into_inner())
        .map_err(|error| StreamError::Setup(format!("audio format: {error:?}")))
    }

    /// Joins the worker once the owner lets go of the stream.
    struct Worker {
        status: Arc<StreamStatus>,
        thread: Option<JoinHandle<()>>,
    }

    impl Drop for Worker {
        fn drop(&mut self) {
            self.status.request_stop();
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    /// Spawns a loop thread and waits for it to report the stream connected.
    fn spawn_worker(
        name: &str,
        status: Arc<StreamStatus>,
        body: impl FnOnce(Arc<StreamStatus>, SyncSender<Result<(), StreamError>>) + Send + 'static,
    ) -> Result<Worker, StreamError> {
        ensure_initialised();
        let (ready_tx, ready_rx) = sync_channel(1);
        let thread_status = Arc::clone(&status);
        let thread = std::thread::Builder::new()
            .name(name.into())
            .spawn(move || body(thread_status, ready_tx))
            .map_err(|error| StreamError::Setup(format!("start stream thread: {error}")))?;
        let worker = Worker {
            status,
            thread: Some(thread),
        };
        match ready_rx.recv_timeout(CONNECT_TIMEOUT.saturating_add(LOOP_TICK)) {
            Ok(Ok(())) => Ok(worker),
            Ok(Err(error)) => Err(error),
            Err(_) => Err(StreamError::Setup("the stream did not connect".into())),
        }
    }

    /// Iterates `mainloop` until the stream gets a node id or fails.
    fn await_node(
        mainloop: &pw::main_loop::MainLoopRc,
        stream: &pw::stream::Stream,
        status: &StreamStatus,
    ) -> Result<u32, StreamError> {
        let deadline = Instant::now().checked_add(CONNECT_TIMEOUT);
        loop {
            let _ = mainloop
                .loop_()
                .iterate(pw::loop_::Timeout::Finite(LOOP_TICK));
            if let Some(failure) = status.failure() {
                return Err(StreamError::Setup(failure));
            }
            let node = stream.node_id();
            if node != 0 && node != u32::MAX {
                return Ok(node);
            }
            if deadline.is_none_or(|deadline| Instant::now() >= deadline) {
                return Err(StreamError::Setup(
                    "PipeWire did not create the stream node".into(),
                ));
            }
        }
    }

    fn record_state(
        status: &StreamStatus,
        old: &pw::stream::StreamState,
        new: &pw::stream::StreamState,
    ) {
        match new {
            pw::stream::StreamState::Error(reason) => status.fail(reason.clone()),
            pw::stream::StreamState::Unconnected
                if !matches!(old, pw::stream::StreamState::Unconnected)
                    && !status.stop_requested() =>
            {
                status.fail("the stream was disconnected");
            }
            _ => {}
        }
    }

    struct Playback {
        worker: Worker,
    }

    impl PlaybackStream for Playback {
        fn timing(&self) -> Option<DeviceTiming> {
            self.worker.status.timing()
        }

        fn failure(&self) -> Option<String> {
            self.worker.status.failure()
        }
    }

    fn playback_graph(
        core: &pw::core::CoreRc,
        expected_cookie: Option<u32>,
        guard: Option<Rc<RefCell<TargetLinkGuard>>>,
        status: Arc<StreamStatus>,
    ) -> Result<(Rc<std::cell::Cell<bool>>, impl Sized), StreamError> {
        let cookie_valid = Rc::new(std::cell::Cell::new(expected_cookie.is_none()));
        let cookie_gate = cookie_valid.clone();
        let core_status = status;
        let core_listener = core
            .add_listener_local()
            .info(move |info| {
                let valid = expected_cookie.is_none_or(|expected| expected == info.cookie());
                cookie_gate.set(valid);
                if !valid {
                    core_status.fail("the observed audio daemon changed");
                }
            })
            .register();
        let registry = core
            .get_registry_rc()
            .map_err(|error| StreamError::Setup(error.to_string()))?;
        let global_guard = guard.clone();
        let removal_guard = guard;
        let links = Rc::new(RefCell::new(std::collections::BTreeMap::new()));
        let added_links = links.clone();
        let registry_listener = registry
            .add_listener_local()
            .global(move |global| {
                let Some(guard) = global_guard.as_ref() else {
                    return;
                };
                match global.type_ {
                    pw::types::ObjectType::Node => guard
                        .borrow_mut()
                        .observe_node(global.id, numeric(global.props, "object.serial")),
                    pw::types::ObjectType::Link => {
                        if let (Some(output), Some(input)) = (
                            numeric(global.props, "link.output.node"),
                            numeric(global.props, "link.input.node"),
                        ) {
                            // Reverse the capture guard: our output must link only to the selected sink.
                            added_links.borrow_mut().insert(global.id, (input, output));
                            guard.borrow_mut().observe_link(input, output);
                        }
                    }
                    _ => {}
                }
            })
            .global_remove(move |id| {
                if let Some(guard) = removal_guard.as_ref() {
                    if let Some((output, input)) = links.borrow_mut().remove(&id) {
                        guard.borrow_mut().remove_link(output, input);
                    }
                    guard.borrow_mut().observe_removal(id);
                }
            })
            .register();
        Ok((cookie_valid, (core_listener, registry, registry_listener)))
    }

    fn wait_playback_graph(
        mainloop: &pw::main_loop::MainLoopRc,
        status: &StreamStatus,
        guard: Option<&Rc<RefCell<TargetLinkGuard>>>,
    ) {
        let link_deadline = Instant::now().checked_add(LINK_TIMEOUT);
        let mut linked = guard.is_none();
        while !status.stop_requested() && status.failure().is_none() {
            let _ = mainloop
                .loop_()
                .iterate(pw::loop_::Timeout::Finite(LOOP_TICK));
            if let Some(guard) = &guard {
                match guard.borrow().verdict() {
                    LinkVerdict::Linked => linked = true,
                    LinkVerdict::Waiting if linked => status.fail("the selected output link ended"),
                    LinkVerdict::Waiting
                        if link_deadline.is_none_or(|deadline| Instant::now() >= deadline) =>
                    {
                        status.fail("the selected output did not connect");
                    }
                    LinkVerdict::Foreign { .. } => {
                        status.fail("the playback stream linked to another output");
                    }
                    LinkVerdict::TargetGone => status.fail("the selected output disappeared"),
                    LinkVerdict::Waiting => {}
                }
            }
        }
    }

    fn run_playback(
        config: &PlaybackConfig,
        source: Box<dyn PlaybackSource>,
        status: &Arc<StreamStatus>,
        ready: &SyncSender<Result<(), StreamError>>,
        observed: Option<super::ObservedPlayback>,
    ) -> Result<(), StreamError> {
        let unavailable = |error: pw::Error| StreamError::Unavailable(error.to_string());
        let setup = |error: pw::Error| StreamError::Setup(error.to_string());
        let mainloop = pw::main_loop::MainLoopRc::new(None).map_err(unavailable)?;
        let context = pw::context::ContextRc::new(&mainloop, None).map_err(unavailable)?;
        let expected_cookie = observed.as_ref().map(|value| value.cookie);
        let guard = observed
            .as_ref()
            .map(|value| Rc::new(RefCell::new(TargetLinkGuard::new(value.serial))));
        let core = match observed {
            Some(value) => context.connect_fd_rc(value.socket, None),
            None => context.connect_rc(None),
        }
        .map_err(unavailable)?;
        let (cookie_valid, _graph) =
            playback_graph(&core, expected_cookie, guard.clone(), status.clone())?;
        let stream =
            pw::stream::StreamBox::new(&core, "tilo-playback", properties(config.properties()))
                .map_err(setup)?;
        let renderer =
            PlaybackRenderer::new(source, config.channels, config.rate, Arc::clone(status));
        let stride = renderer.stride();
        let state_status = Arc::clone(status);
        let process_guard = guard.clone();
        let process_cookie = cookie_valid;
        let _listener = stream
            .add_local_listener_with_user_data(renderer)
            .state_changed(move |_, _, old, new| record_state(&state_status, &old, &new))
            .process(move |stream, renderer| {
                let Some(mut buffer) = stream.dequeue_buffer() else {
                    return;
                };
                let requested = usize::try_from(buffer.requested()).ok();
                let clock = LiveClock { stream, stride };
                let Some(data) = buffer.datas_mut().first_mut() else {
                    return;
                };
                let allowed = process_cookie.get()
                    && process_guard
                        .as_ref()
                        .is_none_or(|guard| guard.borrow().verdict() == LinkVerdict::Linked);
                let written = data.data().map_or(0, |out| {
                    if allowed {
                        renderer.fill(&clock, out, requested)
                    } else {
                        out.fill(0);
                        out.len()
                    }
                });
                let chunk = data.chunk_mut();
                *chunk.offset_mut() = 0;
                *chunk.stride_mut() = i32::try_from(stride).unwrap_or(i32::MAX);
                *chunk.size_mut() = u32::try_from(written).unwrap_or(0);
            })
            .register()
            .map_err(setup)?;
        let format = format_pod(
            spa::param::audio::AudioFormat::F32LE,
            config.rate,
            config.channels,
        )?;
        let pod = Pod::from_bytes(&format)
            .ok_or_else(|| StreamError::Setup("PipeWire rejected the audio format".into()))?;
        stream
            .connect(
                spa::utils::Direction::Output,
                None,
                pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
                &mut [pod],
            )
            .map_err(setup)?;
        let node = await_node(&mainloop, &stream, status)?;
        if let Some(guard) = &guard {
            guard.borrow_mut().set_own_node(node);
        }
        if ready.send(Ok(())).is_err() {
            let _ = stream.disconnect();
            return Ok(());
        }
        wait_playback_graph(&mainloop, status, guard.as_ref());
        let _ = stream.disconnect();
        Ok(())
    }

    struct Capture {
        worker: Worker,
        chunks: Receiver<CaptureChunk>,
    }

    impl CaptureStream for Capture {
        fn drain(&mut self) -> Result<Vec<CaptureChunk>, String> {
            let mut chunks = Vec::new();
            loop {
                match self.chunks.try_recv() {
                    Ok(chunk) => chunks.push(chunk),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        return Err(self
                            .worker
                            .status
                            .failure()
                            .unwrap_or_else(|| "the capture stream ended".into()));
                    }
                }
            }
            match self.worker.status.failure() {
                Some(failure) => Err(failure),
                None => Ok(chunks),
            }
        }
    }

    /// Parses a numeric registry property.
    fn numeric(props: Option<&spa::utils::dict::DictRef>, key: &str) -> Option<u32> {
        props?.get(key)?.parse().ok()
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one function owns the loop, the registry watch and the stream it verifies"
    )]
    fn run_capture(
        config: &CaptureConfig,
        chunks: SyncSender<CaptureChunk>,
        status: &Arc<StreamStatus>,
        ready: &SyncSender<Result<(), StreamError>>,
    ) -> Result<(), StreamError> {
        let unavailable = |error: pw::Error| StreamError::Unavailable(error.to_string());
        let setup = |error: pw::Error| StreamError::Setup(error.to_string());
        let mainloop = pw::main_loop::MainLoopRc::new(None).map_err(unavailable)?;
        let context = pw::context::ContextRc::new(&mainloop, None).map_err(unavailable)?;
        let core = context.connect_rc(None).map_err(unavailable)?;
        let registry = core.get_registry_rc().map_err(setup)?;
        let guard = Rc::new(RefCell::new(TargetLinkGuard::new(config.target.serial)));
        let global_guard = Rc::clone(&guard);
        let removal_guard = Rc::clone(&guard);
        let _registry_listener = registry
            .add_listener_local()
            .global(move |global| {
                let props = global.props;
                match global.type_ {
                    pw::types::ObjectType::Node => global_guard
                        .borrow_mut()
                        .observe_node(global.id, numeric(props, "object.serial")),
                    pw::types::ObjectType::Link => {
                        if let (Some(output), Some(input)) = (
                            numeric(props, "link.output.node"),
                            numeric(props, "link.input.node"),
                        ) {
                            global_guard.borrow_mut().observe_link(output, input);
                        }
                    }
                    _ => {}
                }
            })
            .global_remove(move |id| removal_guard.borrow_mut().observe_removal(id))
            .register();

        let stream =
            pw::stream::StreamBox::new(&core, "tilo-capture", properties(config.properties()))
                .map_err(setup)?;
        let stride = config.stride();
        let rate = config.rate;
        let verified = Rc::new(RefCell::new(false));
        let process_verified = Rc::clone(&verified);
        let state_status = Arc::clone(status);
        let _listener = stream
            .add_local_listener_with_user_data(CaptureTimeline::new(rate))
            .state_changed(move |_, _, old, new| record_state(&state_status, &old, &new))
            .process(move |stream, timeline| {
                let Some(mut buffer) = stream.dequeue_buffer() else {
                    return;
                };
                let timing = stream
                    .time()
                    .ok()
                    .map(|time| {
                        let rate = time.rate();
                        DeviceTiming::from_raw(
                            time.now(),
                            rate.num,
                            rate.denom,
                            time.delay(),
                            0,
                            0,
                            stride,
                        )
                    })
                    .filter(DeviceTiming::is_live);
                let Some(data) = buffer.datas_mut().first_mut() else {
                    return;
                };
                let chunk = data.chunk();
                let offset = usize::try_from(chunk.offset()).unwrap_or(usize::MAX);
                let size = usize::try_from(chunk.size()).unwrap_or(0);
                let Some(bytes) = data.data() else {
                    return;
                };
                let end = offset.saturating_add(size).min(bytes.len());
                let pcm = whole_frames(bytes.get(offset..end).unwrap_or_default(), stride);
                let frames = pcm.len().checked_div(stride).unwrap_or(0);
                if frames == 0 {
                    return;
                }
                let origin_ns = timing.map_or_else(
                    || {
                        let span = frames_to_ns(i64::try_from(frames).unwrap_or(0), rate);
                        monotonic_now_ns().saturating_sub(span)
                    },
                    |timing| timing.capture_origin_ns(frames, rate),
                );
                let first_frame = timeline.stamp(origin_ns, frames);
                if !*process_verified.borrow() {
                    return;
                }
                // A full queue drops this chunk; the timeline already
                // advanced past it, so the receiver sees the gap.
                let _ = chunks.try_send(CaptureChunk {
                    first_frame,
                    pcm_s16le: pcm.to_vec(),
                });
            })
            .register()
            .map_err(setup)?;
        let format = format_pod(
            spa::param::audio::AudioFormat::S16LE,
            config.rate,
            config.channels,
        )?;
        let pod = Pod::from_bytes(&format)
            .ok_or_else(|| StreamError::Setup("PipeWire rejected the audio format".into()))?;
        stream
            .connect(
                spa::utils::Direction::Input,
                None,
                pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
                &mut [pod],
            )
            .map_err(setup)?;
        let node = await_node(&mainloop, &stream, status)?;
        guard.borrow_mut().set_own_node(node);
        if ready.send(Ok(())).is_err() {
            let _ = stream.disconnect();
            return Ok(());
        }
        let link_deadline = Instant::now().checked_add(LINK_TIMEOUT);
        while !status.stop_requested() && status.failure().is_none() {
            let _ = mainloop
                .loop_()
                .iterate(pw::loop_::Timeout::Finite(LOOP_TICK));
            let verdict = guard.borrow().verdict();
            match verdict {
                LinkVerdict::Linked => *verified.borrow_mut() = true,
                LinkVerdict::Waiting if !*verified.borrow() => {
                    if link_deadline.is_none_or(|deadline| Instant::now() >= deadline) {
                        status.fail("the chosen audio node is not available");
                    }
                }
                LinkVerdict::Waiting => {}
                LinkVerdict::Foreign { node } => {
                    *verified.borrow_mut() = false;
                    status.fail(format!(
                        "PipeWire linked node {node} instead of the chosen audio node"
                    ));
                }
                LinkVerdict::TargetGone => {
                    *verified.borrow_mut() = false;
                    status.fail("the chosen audio node went away");
                }
            }
        }
        let _ = stream.disconnect();
        Ok(())
    }

    impl AudioStreams for PipeWireStreams {
        fn open_playback(
            &self,
            config: &PlaybackConfig,
            source: Box<dyn PlaybackSource>,
        ) -> Result<Box<dyn PlaybackStream>, StreamError> {
            let config = config.clone();
            let status = Arc::new(StreamStatus::default());
            let worker = spawn_worker("tilo-pw-playback", status, move |status, ready| {
                if let Err(error) = run_playback(&config, source, &status, &ready, None) {
                    status.fail(error.to_string());
                    let _ = ready.send(Err(error));
                }
            })?;
            Ok(Box::new(Playback { worker }))
        }

        fn open_capture(
            &self,
            config: &CaptureConfig,
        ) -> Result<Box<dyn CaptureStream>, StreamError> {
            let config = config.clone();
            let (chunk_tx, chunk_rx) = sync_channel(config.queue_chunks.max(1));
            let status = Arc::new(StreamStatus::default());
            let worker = spawn_worker("tilo-pw-capture", status, move |status, ready| {
                if let Err(error) = run_capture(&config, chunk_tx, &status, &ready) {
                    status.fail(error.to_string());
                    let _ = ready.send(Err(error));
                }
            })?;
            Ok(Box::new(Capture {
                worker,
                chunks: chunk_rx,
            }))
        }
    }

    pub(super) fn observed_playback(
        config: &PlaybackConfig,
        source: Box<dyn PlaybackSource>,
        observed: super::ObservedPlayback,
    ) -> Result<Box<dyn PlaybackStream>, StreamError> {
        if config.target_serial != Some(observed.serial) {
            return Err(StreamError::Setup(
                "output target differs from observed sink".into(),
            ));
        }
        let config = config.clone();
        let status = Arc::new(StreamStatus::default());
        let worker = spawn_worker("tilo-pw-app-output", status, move |status, ready| {
            if let Err(error) = run_playback(&config, source, &status, &ready, Some(observed)) {
                status.fail(error.to_string());
                let _ = ready.send(Err(error));
            }
        })?;
        Ok(Box::new(Playback { worker }))
    }
}

#[cfg(test)]
#[allow(
    clippy::cast_precision_loss,
    reason = "fixture arithmetic on literal times, frame counts and ramps"
)]
mod tests {
    use super::*;
    use crate::sync_playout::{AudioFrame, LinearClockMapping, Payload, PlayoutConfig};

    const RATE: u32 = 48_000;

    struct FakeClock {
        timing: Option<DeviceTiming>,
        now_ns: i64,
    }

    impl StreamClock for FakeClock {
        fn timing(&self) -> Option<DeviceTiming> {
            self.timing
        }

        fn now_ns(&self) -> i64 {
            self.now_ns
        }
    }

    /// Records the presentation time of each block and writes a ramp.
    struct Recorder {
        seen: Seen,
    }

    impl PlaybackSource for Recorder {
        fn render(&mut self, presentation_ns: i64, out: &mut [f32]) {
            self.seen.lock().unwrap().push((presentation_ns, out.len()));
            for (index, sample) in out.iter_mut().enumerate() {
                *sample = (index % 100) as f32 / 100.0;
            }
        }
    }

    type Seen = Arc<Mutex<Vec<(i64, usize)>>>;

    fn renderer(channels: u16) -> (PlaybackRenderer, Seen, Arc<StreamStatus>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let status = Arc::new(StreamStatus::default());
        let renderer = PlaybackRenderer::new(
            Box::new(Recorder {
                seen: Arc::clone(&seen),
            }),
            channels,
            RATE,
            Arc::clone(&status),
        );
        (renderer, seen, status)
    }

    #[test]
    fn raw_time_converts_graph_ticks_and_stream_frames() {
        // 480 ticks of a 1/48000 graph is 10 ms; 3840 queued bytes of stereo
        // f32 are 480 frames, plus 16 in the resampler.
        let timing = DeviceTiming::from_raw(5_000_000_000, 1, 48_000, 480, 3840, 16, 8);
        assert_eq!(timing.delay_ns, 10_000_000);
        assert_eq!(timing.queued_frames, 496);
        // A 44.1 kHz graph's ticks convert with its own rate.
        let graph = DeviceTiming::from_raw(1, 1, 44_100, 441, 0, 0, 8);
        assert_eq!(graph.delay_ns, 10_000_000);
    }

    #[test]
    fn a_block_is_scheduled_when_it_leaves_the_speaker() {
        let (mut renderer, seen, status) = renderer(2);
        let timing = DeviceTiming {
            now_ns: 7_000_000_000,
            delay_ns: 12_000_000,
            queued_frames: 480,
        };
        let clock = FakeClock {
            timing: Some(timing),
            now_ns: 7_000_500_000,
        };
        let mut out = vec![0_u8; 1024 * 8];
        let written = renderer.fill(&clock, &mut out, Some(256));
        assert_eq!(written, 256 * 8, "only the requested frames are written");
        let (presentation, samples) = seen.lock().unwrap()[0];
        // cycle start + device delay + the 10 ms still queued in the stream.
        assert_eq!(presentation, 7_000_000_000 + 12_000_000 + 10_000_000);
        assert_eq!(samples, 512);
        assert_eq!(
            status.timing(),
            Some(timing),
            "the owner can read the timing"
        );
        let second = f32::from_le_bytes(out[4..8].try_into().unwrap());
        assert!((second - 0.01).abs() < 1e-6, "samples are written as f32le");
    }

    #[test]
    fn before_the_graph_runs_a_block_is_scheduled_for_now() {
        let (mut renderer, seen, status) = renderer(1);
        let clock = FakeClock {
            timing: Some(DeviceTiming::default()),
            now_ns: 42,
        };
        let mut out = vec![0_u8; 64];
        assert_eq!(renderer.fill(&clock, &mut out, None), 64);
        assert_eq!(seen.lock().unwrap()[0], (42, 16));
        assert_eq!(status.timing(), None);
    }

    #[test]
    fn a_buffer_smaller_than_a_frame_renders_nothing() {
        let (mut renderer, seen, _) = renderer(2);
        let clock = FakeClock {
            timing: None,
            now_ns: 1,
        };
        assert_eq!(renderer.fill(&clock, &mut [0; 7], None), 0);
        assert!(seen.lock().unwrap().is_empty());
    }

    #[test]
    fn samples_are_clamped_to_full_scale() {
        struct Loud;
        impl PlaybackSource for Loud {
            fn render(&mut self, _: i64, out: &mut [f32]) {
                out.fill(3.0);
            }
        }
        let mut renderer =
            PlaybackRenderer::new(Box::new(Loud), 1, RATE, Arc::new(StreamStatus::default()));
        let mut out = [0_u8; 4];
        renderer.fill(
            &FakeClock {
                timing: None,
                now_ns: 1,
            },
            &mut out,
            None,
        );
        assert!((f32::from_le_bytes(out) - 1.0).abs() < f32::EPSILON);
    }

    /// A shared playout behind the stream plays a frame at its scheduled
    /// time: the device timing, not the callback time, decides which samples
    /// come out.
    #[test]
    fn a_sync_playout_plays_at_the_device_presentation_time() {
        let latency = 50_000_000;
        let mut playout = SyncPlayout::new(PlayoutConfig::new(RATE, latency, ChannelRole::Left));
        // One second of a constant left channel, starting at leader sample 0.
        let samples = (0..RATE).flat_map(|_| [0.5_f32, -0.25]).collect();
        playout.push(&AudioFrame {
            seq: 1,
            media_ts: 0,
            channels: 2,
            payload: Payload::F32(samples),
        });
        let shared = Arc::new(Mutex::new(SyncPlayoutState {
            playout,
            clock: LinearClockMapping::default(),
        }));
        let status = Arc::new(StreamStatus::default());
        let mut renderer = PlaybackRenderer::new(
            Box::new(SyncPlaybackSource::new(Arc::clone(&shared))),
            1,
            RATE,
            status,
        );
        // The callback runs 30 ms before the group latency elapses, but the
        // device delay is 20 ms and 10 ms are queued, so the block lands at
        // exactly `latency`: the first sample of the stream.
        let clock = FakeClock {
            timing: Some(DeviceTiming {
                now_ns: latency - 30_000_000,
                delay_ns: 20_000_000,
                queued_frames: 480,
            }),
            now_ns: latency - 30_000_000,
        };
        let mut out = vec![0_u8; 480 * 4];
        renderer.fill(&clock, &mut out, None);
        let error = shared.lock().unwrap().playout.stats().presentation_error_ns;
        assert_eq!(error, 0);
        let last = f32::from_le_bytes(out[out.len() - 4..].try_into().unwrap());
        assert!((last - 0.5).abs() < 0.01, "the left channel plays: {last}");
    }

    #[test]
    fn capture_properties_name_the_target_and_forbid_substitutes() {
        let config = CaptureConfig {
            node_name: "tilo-capture\0x".into(),
            target: CaptureTarget {
                serial: 812,
                monitor_sink: true,
            },
            rate: RATE,
            channels: 2,
            queue_chunks: 8,
        };
        let properties = config.properties();
        let get = |key: &str| {
            properties
                .iter()
                .find(|(name, _)| *name == key)
                .map(|(_, value)| value.as_str())
        };
        assert_eq!(get("target.object"), Some("812"));
        assert_eq!(get("node.dont-fallback"), Some("true"));
        assert_eq!(get("node.dont-reconnect"), Some("true"));
        assert_eq!(get("node.dont-move"), Some("true"));
        assert_eq!(get("stream.capture.sink"), Some("true"));
        assert_eq!(get("node.name"), Some("tilo-capturex"));
        let source = CaptureConfig {
            target: CaptureTarget {
                serial: 5,
                monitor_sink: false,
            },
            ..config
        };
        assert!(
            source
                .properties()
                .iter()
                .all(|(key, _)| *key != "stream.capture.sink")
        );
    }

    #[test]
    fn playback_properties_carry_the_role_channels_and_quantum() {
        let config =
            PlaybackConfig::for_role("tilo-member", "Speaker group", RATE, ChannelRole::Right);
        assert_eq!(config.channels, 1);
        let properties = config.properties();
        assert!(properties.contains(&("node.latency", "480/48000".into())));
        assert!(properties.contains(&("audio.channels", "1".into())));
        let stereo =
            PlaybackConfig::for_role("tilo-member", "Speaker group", RATE, ChannelRole::Stereo);
        assert_eq!(stereo.channels, 2);
    }

    #[test]
    fn a_continuous_capture_stays_sample_continuous_through_jitter() {
        let mut timeline = CaptureTimeline::new(RATE);
        let start = 10_000_000_000;
        assert_eq!(timeline.stamp(start, 480), 480_000);
        // The next cycle's timestamp is 300 µs late; the capture continued.
        assert_eq!(timeline.stamp(start + 10_300_000, 480), 480_480);
        assert_eq!(timeline.stamp(start + 19_900_000, 480), 480_960);
        // A 50 ms hole (an xrun) re-anchors on the measured time, leaving a gap.
        let after_gap = timeline.stamp(start + 80_000_000, 480);
        assert_eq!(after_gap, 483_840);
        assert_eq!(timeline.stamp(start + 90_000_000, 480), after_gap + 480);
    }

    #[test]
    fn governed_playback_requires_only_the_observed_output_link() {
        let mut guard = TargetLinkGuard::new(812);
        guard.observe_node(40, Some(812));
        guard.set_own_node(77);
        // Playback registry entries are reversed by the production listener.
        guard.observe_link(40, 77);
        assert_eq!(guard.verdict(), LinkVerdict::Linked);
        guard.remove_link(40, 77);
        assert_eq!(guard.verdict(), LinkVerdict::Waiting);
        guard.observe_link(55, 77);
        assert_eq!(guard.verdict(), LinkVerdict::Foreign { node: 55 });
        guard.observe_link(40, 77);
        assert_eq!(guard.verdict(), LinkVerdict::Foreign { node: 55 });
        guard.observe_removal(40);
        assert_eq!(guard.verdict(), LinkVerdict::TargetGone);
    }

    #[test]
    fn a_link_from_the_target_verifies_the_capture() {
        let mut guard = TargetLinkGuard::new(812);
        assert_eq!(guard.verdict(), LinkVerdict::Waiting);
        guard.observe_node(40, Some(812));
        guard.observe_node(41, Some(900));
        guard.set_own_node(77);
        assert_eq!(guard.verdict(), LinkVerdict::Waiting);
        guard.observe_link(40, 77);
        guard.observe_link(41, 12);
        assert_eq!(guard.verdict(), LinkVerdict::Linked);
    }

    #[test]
    fn a_link_from_any_other_node_fails_the_capture() {
        let mut guard = TargetLinkGuard::new(812);
        guard.observe_node(40, Some(812));
        guard.set_own_node(77);
        // The session manager linked the default source (the microphone).
        guard.observe_link(55, 77);
        assert_eq!(guard.verdict(), LinkVerdict::Foreign { node: 55 });
        guard.observe_link(40, 77);
        assert_eq!(
            guard.verdict(),
            LinkVerdict::Foreign { node: 55 },
            "the chosen node joining too does not excuse the substitute"
        );
    }

    #[test]
    fn a_missing_target_is_never_substituted() {
        let mut guard = TargetLinkGuard::new(812);
        guard.set_own_node(77);
        // The target never appeared; something else was linked anyway.
        guard.observe_link(55, 77);
        assert_eq!(guard.verdict(), LinkVerdict::Foreign { node: 55 });
    }

    #[test]
    fn the_target_leaving_the_graph_fails_the_capture() {
        let mut guard = TargetLinkGuard::new(812);
        guard.observe_node(40, Some(812));
        guard.set_own_node(77);
        guard.observe_link(40, 77);
        guard.observe_removal(40);
        assert_eq!(guard.verdict(), LinkVerdict::TargetGone);
    }

    #[test]
    fn frame_conversions_round_towards_earlier_frames() {
        assert_eq!(frames_to_ns(48_000, RATE), NS_PER_SECOND);
        assert_eq!(ns_to_frames(NS_PER_SECOND, RATE), 48_000);
        assert_eq!(ns_to_frames(20_833, RATE), 0);
        assert_eq!(ns_to_frames(20_834, RATE), 1);
        assert_eq!(ns_to_frames(-1, RATE), -1);
        assert_eq!(whole_frames(&[1, 2, 3, 4, 5, 6], 4), &[1, 2, 3, 4]);
    }

    #[cfg(not(all(feature = "pipewire", target_os = "linux")))]
    #[test]
    fn a_build_without_pipewire_reports_it_instead_of_pretending() {
        let config = PlaybackConfig::for_role("n", "d", RATE, ChannelRole::Stereo);
        let error = native_streams()
            .open_playback(
                &config,
                Box::new(Recorder {
                    seen: Arc::default(),
                }),
            )
            .err()
            .unwrap();
        assert!(matches!(error, StreamError::Unavailable(_)));
    }

    #[test]
    fn a_scheduled_buffer_starts_on_its_frame() {
        // 1000 frames of mono ramp, due at t = 1 s.
        let samples = (0..1000).map(|index| index as f32 / 1000.0).collect();
        let mut buffer = ScheduledBuffer::new(samples, 1, RATE, NS_PER_SECOND);
        let progress = buffer.progress();
        // A block presented 100 frames early: silence, then the ramp.
        let mut out = vec![1.0; 480];
        buffer.render(NS_PER_SECOND - frames_to_ns(100, RATE), &mut out);
        assert!(out[..100].iter().all(|sample| sample.abs() < f32::EPSILON));
        assert!((out[100] - 0.0).abs() < f32::EPSILON);
        assert!((out[101] - 0.001).abs() < 1e-6);
        assert_eq!(progress.late_by_ns(), Some(0), "on time");
        assert!(!progress.finished());
        let mut out = vec![1.0; 480];
        buffer.render(NS_PER_SECOND + frames_to_ns(380, RATE), &mut out);
        assert!((out[0] - 0.38).abs() < 1e-6);
        let mut out = vec![1.0; 480];
        buffer.render(NS_PER_SECOND + frames_to_ns(860, RATE), &mut out);
        assert!((out[139] - 0.999).abs() < 1e-6);
        assert!(out[140..].iter().all(|sample| sample.abs() < f32::EPSILON));
        assert!(progress.finished());
    }

    #[test]
    fn a_scheduled_buffer_that_starts_late_says_so() {
        let mut buffer = ScheduledBuffer::new(vec![0.5; 2000], 2, RATE, NS_PER_SECOND);
        let progress = buffer.progress();
        let mut out = vec![0.0; 960];
        buffer.render(NS_PER_SECOND + 10_000_000, &mut out);
        assert_eq!(progress.late_by_ns(), Some(10_000_000));
    }

    #[test]
    fn a_window_is_cut_from_chunks_and_timed_by_its_first_frame() {
        let pcm = |frames: usize, value: i16| -> Vec<u8> {
            (0..frames).flat_map(|_| value.to_le_bytes()).collect()
        };
        // Mono chunks at frames 48_000.. (t = 1 s), with a 10-frame gap.
        let chunks = vec![
            CaptureChunk {
                first_frame: 47_990,
                pcm_s16le: pcm(100, 7),
            },
            CaptureChunk {
                first_frame: 48_100,
                pcm_s16le: pcm(100, 9),
            },
        ];
        let window =
            assemble_window(&chunks, RATE, 1, NS_PER_SECOND, frames_to_ns(150, RATE)).unwrap();
        assert_eq!(window.first_sample_ns, NS_PER_SECOND);
        assert_eq!(window.samples.len(), 150);
        assert!(window.samples[..90].iter().all(|sample| *sample == 7));
        assert!(
            window.samples[90..100].iter().all(|sample| *sample == 0),
            "the gap is silent"
        );
        assert!(window.samples[100..].iter().all(|sample| *sample == 9));
        // A window before any capture starts at the first captured frame.
        let late = assemble_window(&chunks, RATE, 1, 0, 2 * NS_PER_SECOND).unwrap();
        assert_eq!(late.first_sample_ns, frames_to_ns(47_990, RATE));
        assert!(assemble_window(&chunks, RATE, 1, 0, NS_PER_SECOND / 2).is_none());
    }

    #[test]
    fn capture_is_timed_from_the_start_of_the_delivered_buffer() {
        // The cycle ends at 2 s; the device delay is 5 ms and the buffer holds 480 frames (10 ms).
        let timing = DeviceTiming {
            now_ns: 2 * NS_PER_SECOND,
            delay_ns: 5_000_000,
            queued_frames: 0,
        };
        assert_eq!(
            timing.capture_origin_ns(480, RATE),
            2 * NS_PER_SECOND - 15_000_000
        );
    }

    #[test]
    fn a_targeted_playback_never_falls_back() {
        let mut config = PlaybackConfig::for_role("n", "d", RATE, ChannelRole::Mono);
        assert!(
            config
                .properties()
                .iter()
                .all(|(key, _)| *key != "target.object")
        );
        config.target_serial = Some(33);
        let properties = config.properties();
        assert!(properties.contains(&("target.object", "33".into())));
        assert!(properties.contains(&("node.dont-fallback", "true".into())));
    }

    #[test]
    fn the_first_failure_is_the_one_reported() {
        let status = StreamStatus::default();
        status.fail("target gone");
        status.fail("disconnected");
        assert_eq!(status.failure().as_deref(), Some("target gone"));
    }
}
