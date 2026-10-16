//! Presentation-time playout for synchronized speaker groups.
//!
//! A group leader stamps every audio frame with its media timestamp: the index
//! of its first sample on the leader's clock, so the frame's leader time is
//! `media_ts / sample_rate`. Every member plays that sample at the same
//! instant: `leader time + group latency + member trim`, converted to the
//! member's local clock through a [`ClockMapping`] the caller supplies (the
//! clock estimator lives outside this crate and plugs in through the trait).
//!
//! [`SyncPlayout`] is the member side:
//!
//! * a jitter buffer ordered by media timestamp that ignores duplicates,
//!   drops frames whose playout time has passed, conceals losses with a short
//!   fade to silence, and never waits for a missing frame;
//! * a windowed-sinc polyphase resampler whose ratio a PI controller trims
//!   within ±500 ppm of nominal, so the presentation error converges to zero
//!   without audible pitch change; only an error beyond 20 ms is corrected with
//!   a hard skip or an inserted silence;
//! * a [`ChannelRole`] mapping that renders a stereo source as what this
//!   member plays: one side of a stereo pair, a mono or centre downmix, a
//!   low-passed LFE feed, or the stereo signal itself.
//!
//! Everything here is pure computation over sample buffers; the audio device
//! callback drives [`SyncPlayout::render`] with the local time at which the
//! first rendered sample will leave the speaker.

use std::collections::{BTreeMap, VecDeque};
use std::f64::consts::PI;

/// Nanoseconds per second.
const NS_PER_SEC: i64 = 1_000_000_000;

/// Half the number of resampler taps: each output sample reads this many
/// source samples on either side of its position.
const HALF_TAPS: usize = 16;
/// Resampler taps per output sample.
const TAPS: usize = HALF_TAPS * 2;
/// Fractional positions tabulated between two source samples; coefficients
/// between two tabulated phases are interpolated linearly.
const PHASES: usize = 512;
/// Coefficients in the resampler table: one row of taps per tabulated phase,
/// both ends included.
const TABLE_LEN: usize = 513 * TAPS;
/// Kaiser window shape. 9 puts the stop band near -90 dB.
const KAISER_BETA: f64 = 9.0;
/// Resampler cutoff as a fraction of the sample rate (0.5 is Nyquist). The
/// ratio never leaves 1 ± 500 ppm, so the band edge only has to protect the
/// top of the audible range from imaging.
const CUTOFF: f64 = 0.45;

/// Largest deviation of the resampling ratio from 1. At 500 ppm a pitch change
/// is under a hundredth of a semitone, far below what a listener hears.
pub const MAX_RATIO_DEVIATION: f64 = 500e-6;
/// A presentation error beyond this is corrected at once, by skipping source
/// or inserting silence, instead of by resampling.
pub const HARD_CORRECTION_NS: i64 = 20_000_000;

/// Natural frequency of the drift loop. Half a hertz follows a clock estimate
/// that settles over seconds while ignoring block-to-block noise.
const LOOP_NATURAL_HZ: f64 = 0.5;
/// Damping of the drift loop: critically damped enough not to ring.
const LOOP_DAMPING: f64 = 0.707;

/// Length of the fade at the edge of a gap, in seconds: long enough not to
/// click, short enough to keep concealment tight.
const FADE_SECONDS: f64 = 0.002;

/// Corner frequency of the LFE feed's low-pass filter.
pub const LFE_CUTOFF_HZ: f64 = 120.0;

/// Converts between the group leader's clock and this device's clock.
///
/// Both clocks count nanoseconds. An implementation is typically an offset and
/// a skew fitted by a clock estimator; the playout only needs the two
/// conversions and calls them once per rendered block.
pub trait ClockMapping {
    /// The local time at which the leader's clock reads `leader_ns`.
    fn leader_to_local_ns(&self, leader_ns: i64) -> i64;
    /// The leader's clock reading at local time `local_ns`.
    fn local_to_leader_ns(&self, local_ns: i64) -> i64;
}

/// An offset-and-skew clock mapping:
/// `local = offset + leader · (1 + skew_ppm · 10⁻⁶)`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LinearClockMapping {
    pub offset_ns: i64,
    /// How much faster the local clock runs than the leader's, in parts per
    /// million.
    pub skew_ppm: f64,
}

impl ClockMapping for LinearClockMapping {
    fn leader_to_local_ns(&self, leader_ns: i64) -> i64 {
        let drift = round_to_i64(i64_to_f64(leader_ns) * self.skew_ppm * 1e-6);
        leader_ns
            .saturating_add(self.offset_ns)
            .saturating_add(drift)
    }

    fn local_to_leader_ns(&self, local_ns: i64) -> i64 {
        let since_offset = local_ns.saturating_sub(self.offset_ns);
        let skew = self.skew_ppm * 1e-6;
        let drift = round_to_i64(i64_to_f64(since_offset) * skew / (1.0 + skew));
        since_offset.saturating_sub(drift)
    }
}

/// Which part of a stereo source a group member plays.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChannelRole {
    /// The left channel of a stereo pair.
    Left,
    /// The right channel of a stereo pair.
    Right,
    /// A single speaker covering the whole source: `(L + R) / 2`.
    Mono,
    /// The middle speaker of a room: the mid signal `(L + R) / 2`.
    Center,
    /// A subwoofer: the mid signal low-passed at [`LFE_CUTOFF_HZ`].
    Lfe,
    /// Both channels unchanged, for a member that is itself stereo.
    Stereo,
}

impl ChannelRole {
    /// Interleaved channels [`SyncPlayout::render`] writes for this role.
    pub const fn channels(self) -> usize {
        match self {
            Self::Stereo => 2,
            Self::Left | Self::Right | Self::Mono | Self::Center | Self::Lfe => 1,
        }
    }
}

/// Interleaved samples as they arrive from the leader.
#[derive(Clone, Debug, PartialEq)]
pub enum Payload {
    /// Floating point in `-1.0..=1.0`.
    F32(Vec<f32>),
    /// Signed 16-bit PCM.
    I16(Vec<i16>),
}

impl Payload {
    fn len(&self) -> usize {
        match self {
            Self::F32(samples) => samples.len(),
            Self::I16(samples) => samples.len(),
        }
    }

    fn sample(&self, index: usize) -> f32 {
        match self {
            Self::F32(samples) => samples.get(index).copied().unwrap_or(0.0),
            Self::I16(samples) => samples
                .get(index)
                .map_or(0.0, |&sample| f32::from(sample) / 32_768.0),
        }
    }
}

/// One frame of the leader's stream.
#[derive(Clone, Debug, PartialEq)]
pub struct AudioFrame {
    /// Sender sequence number, reported back in statistics only; ordering and
    /// duplicate detection use `media_ts`.
    pub seq: u64,
    /// Index of the frame's first sample on the leader's clock.
    pub media_ts: i64,
    /// Interleaved channels in `payload`. One channel is played as both sides
    /// of a stereo source; beyond two, only the first two are used.
    pub channels: u16,
    pub payload: Payload,
}

/// What [`SyncPlayout::push`] did with a frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PushOutcome {
    /// Buffered for playout.
    Queued,
    /// A frame with the same media timestamp is already buffered.
    Duplicate,
    /// Its samples were due before it arrived.
    Late,
    /// No channels, or a payload that is not a whole number of samples.
    Malformed,
}

/// Counters and the current state of the drift loop.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PlayoutStats {
    pub queued: u64,
    pub duplicates: u64,
    pub late: u64,
    pub malformed: u64,
    /// Frames discarded because the buffer exceeded its capacity.
    pub overflowed: u64,
    /// Sequence number of the most recently queued frame.
    pub last_seq: Option<u64>,
    /// Source samples played as concealment because no frame covered them.
    pub concealed_samples: u64,
    /// Hard corrections that skipped source to catch up.
    pub skips: u64,
    /// Hard corrections that inserted silence to wait.
    pub inserts: u64,
    /// Presentation error at the start of the last rendered block: positive
    /// when this member plays ahead of the group.
    pub presentation_error_ns: i64,
    /// Source samples consumed per output sample in the last block.
    pub ratio: f64,
}

/// How a member plays the group's stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlayoutConfig {
    /// Sample rate of both the stream and the output device.
    pub sample_rate: u32,
    /// Delay from a sample's leader time to its playout, shared by the whole
    /// group; it must cover network jitter.
    pub group_latency_ns: i64,
    /// This member's own additional delay, from acoustic calibration.
    pub trim_ns: i64,
    pub role: ChannelRole,
    /// Most audio the jitter buffer holds; the oldest frames are discarded
    /// beyond it.
    pub capacity_ns: i64,
}

impl PlayoutConfig {
    /// A configuration with no trim and room for twice the group latency plus
    /// 100 ms.
    pub fn new(sample_rate: u32, group_latency_ns: i64, role: ChannelRole) -> Self {
        Self {
            sample_rate,
            group_latency_ns,
            trim_ns: 0,
            role,
            capacity_ns: group_latency_ns
                .saturating_mul(2)
                .saturating_add(100_000_000),
        }
    }
}

/// A position in the source stream: a whole sample index and the fraction of
/// the way to the next sample. Split so that a large index keeps sub-sample
/// precision.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Position {
    index: i64,
    fraction: f64,
}

impl Position {
    fn advance(&mut self, by: f64) {
        let sum = self.fraction + by;
        let whole = sum.floor();
        self.index = self.index.saturating_add(round_to_i64(whole));
        self.fraction = sum - whole;
    }

    /// `self - other` in samples.
    fn minus(self, other: Self) -> f64 {
        i64_to_f64(self.index.saturating_sub(other.index)) + self.fraction - other.fraction
    }
}

/// A second-order IIR section (transposed direct form II).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Biquad {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
    z1: f64,
    z2: f64,
}

impl Biquad {
    /// Low-pass section from the audio EQ cookbook.
    fn low_pass(sample_rate: f64, corner_hz: f64, q: f64) -> Self {
        let w0 = 2.0 * PI * corner_hz / sample_rate;
        let alpha = w0.sin() / (2.0 * q);
        let cos = w0.cos();
        let a0 = 1.0 + alpha;
        Self {
            b0: (1.0 - cos) / 2.0 / a0,
            b1: (1.0 - cos) / a0,
            b2: (1.0 - cos) / 2.0 / a0,
            a1: -2.0 * cos / a0,
            a2: (1.0 - alpha) / a0,
            z1: 0.0,
            z2: 0.0,
        }
    }

    fn process(&mut self, input: f64) -> f64 {
        let output = self.b0 * input + self.z1;
        self.z1 = self.b1 * input - self.a1 * output + self.z2;
        self.z2 = self.b2 * input - self.a2 * output;
        output
    }
}

/// Fourth-order Butterworth low-pass: two cookbook sections with the
/// Butterworth pole Qs.
fn lfe_filter(sample_rate: f64) -> [Biquad; 2] {
    [
        Biquad::low_pass(sample_rate, LFE_CUTOFF_HZ, 0.541_196_1),
        Biquad::low_pass(sample_rate, LFE_CUTOFF_HZ, 1.306_563),
    ]
}

/// A buffered frame, already converted to stereo `f32`.
#[derive(Clone, Debug)]
struct StoredFrame {
    samples: Vec<[f32; 2]>,
}

/// The member side of a synchronized speaker group; see the module
/// documentation.
#[derive(Debug)]
pub struct SyncPlayout {
    config: PlayoutConfig,
    /// `(PHASES + 1) · TAPS` resampler coefficients, one row per phase.
    coefficients: Vec<f32>,
    frames: BTreeMap<i64, StoredFrame>,
    /// Role-mapped source samples, interleaved by the role's channel count,
    /// starting at source index `history_start`.
    history: VecDeque<f32>,
    history_start: i64,
    /// Source samples to fade in after a gap; counts down.
    fade_in_remaining: usize,
    fade_len: usize,
    /// Next source position to render; `None` before the first block.
    position: Option<Position>,
    integrator: f64,
    /// Output samples of silence still to insert for a hard correction.
    hold_remaining: usize,
    /// Output samples still to fade in after a hard correction.
    output_fade_remaining: usize,
    lfe: [Biquad; 2],
    scratch: Vec<[f32; 2]>,
    stats: PlayoutStats,
}

impl SyncPlayout {
    pub fn new(config: PlayoutConfig) -> Self {
        let rate = f64::from(config.sample_rate.max(1));
        Self {
            config,
            coefficients: sinc_table(),
            frames: BTreeMap::new(),
            history: VecDeque::new(),
            history_start: 0,
            fade_in_remaining: 0,
            fade_len: round_to_usize(rate * FADE_SECONDS).max(1),
            position: None,
            integrator: 0.0,
            hold_remaining: 0,
            output_fade_remaining: 0,
            lfe: lfe_filter(rate),
            scratch: Vec::new(),
            stats: PlayoutStats {
                ratio: 1.0,
                ..PlayoutStats::default()
            },
        }
    }

    pub const fn config(&self) -> &PlayoutConfig {
        &self.config
    }

    pub const fn stats(&self) -> &PlayoutStats {
        &self.stats
    }

    /// Changes this member's trim. A change under [`HARD_CORRECTION_NS`] is
    /// absorbed by resampling, without a gap.
    pub const fn set_trim_ns(&mut self, trim_ns: i64) {
        self.config.trim_ns = trim_ns;
    }

    /// Changes the group latency, absorbed like a trim change.
    pub const fn set_group_latency_ns(&mut self, latency_ns: i64) {
        self.config.group_latency_ns = latency_ns;
    }

    /// Audio buffered ahead of the playout position, in nanoseconds.
    #[cfg(test)]
    pub fn buffered_ns(&self) -> i64 {
        let Some((&start, last)) = self.frames.last_key_value() else {
            return 0;
        };
        let end = start.saturating_add(len_i64(last.samples.len()));
        let from = self.position.map_or_else(
            || {
                self.frames
                    .first_key_value()
                    .map_or(end, |(&first, _)| first)
            },
            |position| position.index,
        );
        self.samples_to_ns(end.saturating_sub(from).max(0))
    }

    /// Accepts a frame from the network.
    pub fn push(&mut self, frame: &AudioFrame) -> PushOutcome {
        let outcome = self.queue(frame);
        match outcome {
            PushOutcome::Queued => self.stats.queued = self.stats.queued.saturating_add(1),
            PushOutcome::Duplicate => {
                self.stats.duplicates = self.stats.duplicates.saturating_add(1);
            }
            PushOutcome::Late => self.stats.late = self.stats.late.saturating_add(1),
            PushOutcome::Malformed => {
                self.stats.malformed = self.stats.malformed.saturating_add(1);
            }
        }
        outcome
    }

    fn queue(&mut self, frame: &AudioFrame) -> PushOutcome {
        let channels = usize::from(frame.channels);
        let len = frame.payload.len();
        let Some(count) = len
            .checked_div(channels)
            .filter(|count| count.saturating_mul(channels) == len)
        else {
            return PushOutcome::Malformed;
        };
        let end = frame.media_ts.saturating_add(len_i64(count));
        if self.position.is_some() && end <= self.history_end() {
            return PushOutcome::Late;
        }
        if self.frames.contains_key(&frame.media_ts) {
            return PushOutcome::Duplicate;
        }
        let right = usize::from(channels > 1);
        let samples = (0..count)
            .map(|at| {
                let base = at.saturating_mul(channels);
                [
                    frame.payload.sample(base),
                    frame.payload.sample(base.saturating_add(right)),
                ]
            })
            .collect();
        self.frames.insert(frame.media_ts, StoredFrame { samples });
        self.stats.last_seq = Some(frame.seq);
        self.enforce_capacity();
        PushOutcome::Queued
    }

    fn enforce_capacity(&mut self) {
        let capacity = self.ns_to_samples(self.config.capacity_ns);
        loop {
            let (Some((&first, _)), Some((&last, last_frame))) =
                (self.frames.first_key_value(), self.frames.last_key_value())
            else {
                return;
            };
            let end = last.saturating_add(len_i64(last_frame.samples.len()));
            if end.saturating_sub(first) <= capacity || self.frames.len() < 2 {
                return;
            }
            self.frames.pop_first();
            self.stats.overflowed = self.stats.overflowed.saturating_add(1);
        }
    }

    /// Renders `out` (interleaved, [`ChannelRole::channels`] per sample
    /// frame), whose first sample leaves the speaker at local time
    /// `local_now_ns`.
    ///
    /// Always fills the whole buffer: missing audio becomes silence.
    pub fn render(&mut self, clock: &dyn ClockMapping, local_now_ns: i64, out: &mut [f32]) {
        let channels = self.config.role.channels();
        let count = out.len().checked_div(channels).unwrap_or(0);
        if count == 0 {
            return;
        }
        let rate = f64::from(self.config.sample_rate.max(1));
        let desired = self.desired_position(clock, local_now_ns);
        let block_ns = self.samples_to_ns(len_i64(count));
        let desired_end = self.desired_position(clock, local_now_ns.saturating_add(block_ns));
        let feed_forward = desired_end.minus(desired) / usize_to_f64(count);

        let mut position = self.position.unwrap_or(desired);
        let error = position.minus(desired);
        self.stats.presentation_error_ns = round_to_i64(error / rate * 1e9);
        let hard_limit = i64_to_f64(self.ns_to_samples(HARD_CORRECTION_NS));
        if self.hold_remaining == 0 && error.abs() > hard_limit {
            self.integrator = 0.0;
            self.output_fade_remaining = self.fade_len;
            if error < 0.0 {
                position = desired;
                self.stats.skips = self.stats.skips.saturating_add(1);
            } else {
                self.hold_remaining = round_to_usize(error);
                self.stats.inserts = self.stats.inserts.saturating_add(1);
            }
        }
        let ratio = if self.hold_remaining > 0 {
            feed_forward
        } else {
            self.steer(error / rate, feed_forward, usize_to_f64(count) / rate)
        };
        self.stats.ratio = ratio;

        self.prune_history(position.index);
        let reach = round_to_i64((usize_to_f64(count) * ratio + position.fraction).ceil());
        self.ensure_history(
            position
                .index
                .saturating_add(reach)
                .saturating_add(len_i64(HALF_TAPS))
                .saturating_add(1),
        );

        for frame in out.chunks_exact_mut(channels) {
            if self.hold_remaining > 0 {
                self.hold_remaining = self.hold_remaining.saturating_sub(1);
                frame.fill(0.0);
                continue;
            }
            let gain = if self.output_fade_remaining > 0 {
                self.output_fade_remaining = self.output_fade_remaining.saturating_sub(1);
                1.0 - usize_to_f32(self.output_fade_remaining) / usize_to_f32(self.fade_len)
            } else {
                1.0
            };
            for (channel, sample) in frame.iter_mut().enumerate() {
                *sample = self.interpolate(position, channel, channels) * gain;
            }
            position.advance(ratio);
        }

        self.position = Some(position);
        self.prune_history(position.index);
        let consumed = self.history_end();
        self.frames
            .retain(|&start, frame| start.saturating_add(len_i64(frame.samples.len())) > consumed);
    }

    /// PI controller on the presentation error (seconds, positive when ahead):
    /// returns the resampling ratio for the next block.
    fn steer(&mut self, error_s: f64, feed_forward: f64, block_s: f64) -> f64 {
        let omega = 2.0 * PI * LOOP_NATURAL_HZ;
        let kp = 2.0 * LOOP_DAMPING * omega;
        let ki = omega * omega;
        let integrated = self.integrator + error_s * block_s;
        let unclamped = feed_forward * (1.0 - kp * error_s - ki * integrated);
        let ratio = unclamped.clamp(1.0 - MAX_RATIO_DEVIATION, 1.0 + MAX_RATIO_DEVIATION);
        // Integrate only while the ratio is inside its range, so a long
        // saturation does not wind the integrator up.
        if (ratio - unclamped).abs() < f64::EPSILON {
            self.integrator = integrated;
        }
        ratio
    }

    /// The source position that should be leaving the speaker at `local_ns`.
    fn desired_position(&self, clock: &dyn ClockMapping, local_ns: i64) -> Position {
        let leader_ns = clock.local_to_leader_ns(local_ns);
        let media_ns = leader_ns
            .saturating_sub(self.config.group_latency_ns)
            .saturating_sub(self.config.trim_ns);
        let scaled = i128::from(media_ns).saturating_mul(i128::from(self.config.sample_rate));
        let ns = i128::from(NS_PER_SEC);
        Position {
            index: i64::try_from(scaled.div_euclid(ns)).unwrap_or(i64::MAX),
            fraction: i64_to_f64(i64::try_from(scaled.rem_euclid(ns)).unwrap_or(0))
                / i64_to_f64(NS_PER_SEC),
        }
    }

    fn samples_to_ns(&self, samples: i64) -> i64 {
        let scaled = i128::from(samples).saturating_mul(i128::from(NS_PER_SEC));
        let rate = i128::from(self.config.sample_rate.max(1));
        i64::try_from(scaled.checked_div(rate).unwrap_or(0)).unwrap_or(i64::MAX)
    }

    fn ns_to_samples(&self, ns: i64) -> i64 {
        let scaled = i128::from(ns).saturating_mul(i128::from(self.config.sample_rate));
        i64::try_from(scaled.checked_div(i128::from(NS_PER_SEC)).unwrap_or(0)).unwrap_or(i64::MAX)
    }

    fn history_end(&self) -> i64 {
        let frames = self
            .history
            .len()
            .checked_div(self.config.role.channels())
            .unwrap_or(0);
        self.history_start.saturating_add(len_i64(frames))
    }

    /// Drops history the resampler no longer reads for `index`, or restarts
    /// it there when `index` lies outside it.
    fn prune_history(&mut self, index: i64) {
        let front = index.saturating_sub(len_i64(HALF_TAPS)).saturating_add(1);
        let end = self.history_end();
        if front < self.history_start || front > end {
            self.history.clear();
            self.history_start = front;
            return;
        }
        let drop = usize::try_from(front.saturating_sub(self.history_start)).unwrap_or(0);
        let values = drop.saturating_mul(self.config.role.channels());
        self.history.drain(..values.min(self.history.len()));
        self.history_start = front;
    }

    /// Extends the history up to (not including) source index `upto`, from
    /// buffered frames where they exist and concealment where they do not.
    fn ensure_history(&mut self, upto: i64) {
        let mut scratch = std::mem::take(&mut self.scratch);
        loop {
            let at = self.history_end();
            if at >= upto {
                break;
            }
            scratch.clear();
            let covering = self
                .frames
                .range(..=at)
                .next_back()
                .filter(|(start, frame)| start.saturating_add(len_i64(frame.samples.len())) > at);
            if let Some((&start, frame)) = covering {
                let len = len_i64(frame.samples.len());
                let end = start.saturating_add(len);
                let continues = self.frames.contains_key(&end);
                let offset = usize::try_from(at.saturating_sub(start)).unwrap_or(0);
                let take = usize::try_from(upto.min(end).saturating_sub(at)).unwrap_or(0);
                for (index, sample) in frame.samples.iter().enumerate().skip(offset).take(take) {
                    let mut gain = if self.fade_in_remaining > 0 {
                        self.fade_in_remaining = self.fade_in_remaining.saturating_sub(1);
                        1.0 - usize_to_f32(self.fade_in_remaining) / usize_to_f32(self.fade_len)
                    } else {
                        1.0
                    };
                    let remaining = frame.samples.len().saturating_sub(index);
                    if !continues && remaining <= self.fade_len {
                        // The next frame is not here with its deadline a
                        // fade away: fade out rather than cut off.
                        gain *=
                            usize_to_f32(remaining.saturating_sub(1)) / usize_to_f32(self.fade_len);
                    }
                    scratch.push([sample[0] * gain, sample[1] * gain]);
                }
                if !continues && end <= upto {
                    self.fade_in_remaining = self.fade_len;
                }
            } else {
                let next = self
                    .frames
                    .range(at.saturating_add(1)..)
                    .next()
                    .map_or(upto, |(&start, _)| start);
                let gap = usize::try_from(next.min(upto).saturating_sub(at)).unwrap_or(0);
                scratch.resize(gap, [0.0, 0.0]);
                self.fade_in_remaining = self.fade_len;
                self.stats.concealed_samples = self
                    .stats
                    .concealed_samples
                    .saturating_add(u64::try_from(gap).unwrap_or(u64::MAX));
            }
            if scratch.is_empty() {
                break;
            }
            self.append_mapped(&scratch);
        }
        self.scratch = scratch;
    }

    /// Applies the channel role to stereo source samples and appends them to
    /// the history.
    fn append_mapped(&mut self, samples: &[[f32; 2]]) {
        for &[left, right] in samples {
            let mid = 0.5 * (left + right);
            match self.config.role {
                ChannelRole::Left => self.history.push_back(left),
                ChannelRole::Right => self.history.push_back(right),
                ChannelRole::Mono | ChannelRole::Center => self.history.push_back(mid),
                ChannelRole::Lfe => {
                    let filtered = self
                        .lfe
                        .iter_mut()
                        .fold(f64::from(mid), |signal, section| section.process(signal));
                    self.history.push_back(f64_to_f32(filtered));
                }
                ChannelRole::Stereo => {
                    self.history.push_back(left);
                    self.history.push_back(right);
                }
            }
        }
    }

    /// Band-limited interpolation of one channel at `position`.
    fn interpolate(&self, position: Position, channel: usize, channels: usize) -> f32 {
        let phase = position.fraction * usize_to_f64(PHASES);
        let lower = round_to_usize(phase.floor()).min(PHASES.saturating_sub(1));
        let blend = f64_to_f32(phase - usize_to_f64(lower));
        let row = |phase: usize| {
            let start = phase.saturating_mul(TAPS);
            self.coefficients
                .get(start..start.saturating_add(TAPS))
                .unwrap_or(&[])
        };
        let (low, high) = (row(lower), row(lower.saturating_add(1)));
        let first = position
            .index
            .saturating_sub(len_i64(HALF_TAPS))
            .saturating_add(1)
            .saturating_sub(self.history_start);
        let Ok(first) = usize::try_from(first) else {
            return 0.0;
        };
        low.iter()
            .zip(high)
            .enumerate()
            .map(|(tap, (&a, &b))| {
                let slot = first
                    .saturating_add(tap)
                    .saturating_mul(channels)
                    .saturating_add(channel);
                let sample = self.history.get(slot).copied().unwrap_or(0.0);
                sample * (a + (b - a) * blend)
            })
            .sum()
    }
}

/// Kaiser-windowed sinc coefficients for `PHASES + 1` fractional positions,
/// each row normalized to unit DC gain.
fn sinc_table() -> Vec<f32> {
    let half = usize_to_f64(HALF_TAPS);
    let norm = bessel_i0(KAISER_BETA);
    let mut table = Vec::with_capacity(TABLE_LEN);
    for phase in 0..=PHASES {
        let fraction = usize_to_f64(phase) / usize_to_f64(PHASES);
        let row: Vec<f64> = (0..TAPS)
            .map(|tap| {
                let t = usize_to_f64(tap) - (half - 1.0) - fraction;
                let x = 2.0 * CUTOFF * t;
                let sinc = if x.abs() < 1e-12 {
                    1.0
                } else {
                    (PI * x).sin() / (PI * x)
                };
                let edge = (t / half).clamp(-1.0, 1.0);
                let window = bessel_i0(KAISER_BETA * (1.0 - edge * edge).sqrt()) / norm;
                sinc * window
            })
            .collect();
        let sum: f64 = row.iter().sum();
        table.extend(row.iter().map(|&c| f64_to_f32(c / sum)));
    }
    table
}

/// Modified Bessel function of the first kind, order zero (power series).
fn bessel_i0(x: f64) -> f64 {
    let quarter = x * x / 4.0;
    let mut term = 1.0;
    let mut sum = 1.0;
    for k in 1..64_u32 {
        term *= quarter / f64::from(k).powi(2);
        sum += term;
        if term < sum * 1e-17 {
            break;
        }
    }
    sum
}

#[allow(
    clippy::cast_precision_loss,
    reason = "stream positions stay far below 2^52 samples"
)]
const fn i64_to_f64(value: i64) -> f64 {
    value as f64
}

#[allow(
    clippy::cast_precision_loss,
    reason = "buffer lengths stay far below 2^52"
)]
const fn usize_to_f64(value: usize) -> f64 {
    value as f64
}

#[allow(
    clippy::cast_precision_loss,
    reason = "fade lengths are a few hundred samples"
)]
const fn usize_to_f32(value: usize) -> f32 {
    value as f32
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "audio samples are narrowed to the output format on purpose"
)]
const fn f64_to_f32(value: f64) -> f32 {
    value as f32
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "a saturating float-to-int cast is the intended rounding"
)]
fn round_to_i64(value: f64) -> i64 {
    value.round() as i64
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a saturating float-to-int cast is the intended rounding; negatives clamp to 0"
)]
fn round_to_usize(value: f64) -> usize {
    value.round().max(0.0) as usize
}

fn len_i64(len: usize) -> i64 {
    i64::try_from(len).unwrap_or(i64::MAX)
}

#[cfg(test)]
#[allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    reason = "test signal synthesis and measurement over small, known ranges"
)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;
    const FRAME: usize = 240;
    const BLOCK: usize = 256;
    const LATENCY_NS: i64 = 60_000_000;

    /// Deterministic xorshift generator; the tests must not depend on a seed
    /// source.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        fn unit(&mut self) -> f64 {
            (self.next() >> 11) as f64 / (1_u64 << 53) as f64
        }
    }

    #[derive(Clone, Copy)]
    struct Network {
        loss: f64,
        duplicate: f64,
        /// Probability that a frame is held back far beyond the latency.
        very_late: f64,
        min_delay_ns: i64,
        max_delay_ns: i64,
    }

    const CLEAN_JITTER: Network = Network {
        loss: 0.0,
        duplicate: 0.05,
        very_late: 0.0,
        min_delay_ns: 2_000_000,
        max_delay_ns: 30_000_000,
    };

    struct Run {
        output: Vec<f32>,
        /// Local time of each output sample frame.
        times: Vec<i64>,
        /// Presentation error reported at each block.
        errors_ns: Vec<i64>,
        max_buffered_ns: i64,
        stats: PlayoutStats,
    }

    fn stereo_tone(start: i64, count: usize, left_hz: f64, right_hz: f64) -> Vec<f32> {
        (0..count)
            .flat_map(|i| {
                let t = (start + i as i64) as f64 / f64::from(RATE);
                [
                    (0.5 * (2.0 * PI * left_hz * t).sin()) as f32,
                    (0.5 * (2.0 * PI * right_hz * t).sin()) as f32,
                ]
            })
            .collect()
    }

    /// Streams `seconds` of a stereo tone from a leader whose clock relates to
    /// the member's through `clock`, over `network`, into a member whose
    /// output device runs `dac_ppm` fast against its own clock.
    fn simulate(
        role: ChannelRole,
        clock: &mut dyn FnMut(f64) -> LinearClockMapping,
        network: Network,
        dac_ppm: f64,
        seconds: f64,
        tones: (f64, f64),
    ) -> Run {
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
        let mut playout = SyncPlayout::new(PlayoutConfig::new(RATE, LATENCY_NS, role));
        let channels = role.channels();
        let start_local: i64 = 5_000_000_000;
        let blocks = (seconds * f64::from(RATE) / BLOCK as f64) as usize;
        let mut in_flight: Vec<(i64, AudioFrame)> = Vec::new();
        let mut next_ts: i64 = {
            let mapping = clock(0.0);
            mapping.local_to_leader_ns(start_local) * i64::from(RATE) / NS_PER_SEC
        };
        let mut seq = 0;
        let mut run = Run {
            output: Vec::new(),
            times: Vec::new(),
            errors_ns: Vec::new(),
            max_buffered_ns: 0,
            stats: PlayoutStats::default(),
        };
        let block_ns = (BLOCK as f64 * 1e9 / f64::from(RATE) / (1.0 + dac_ppm * 1e-6)) as i64;
        for block in 0..blocks {
            let local_now = start_local + block as i64 * block_ns;
            let elapsed = block as f64 * BLOCK as f64 / f64::from(RATE);
            let mapping = clock(elapsed);
            // The leader sends each frame once its last sample exists on its
            // clock, reading the true relationship between the clocks.
            let leader_now = mapping.local_to_leader_ns(local_now + block_ns);
            while (next_ts + FRAME as i64) * NS_PER_SEC / i64::from(RATE) <= leader_now {
                let sent_leader = (next_ts + FRAME as i64) * NS_PER_SEC / i64::from(RATE);
                let sent_local = mapping.leader_to_local_ns(sent_leader);
                let frame = AudioFrame {
                    seq,
                    media_ts: next_ts,
                    channels: 2,
                    payload: Payload::F32(stereo_tone(next_ts, FRAME, tones.0, tones.1)),
                };
                seq += 1;
                next_ts += FRAME as i64;
                if rng.unit() < network.loss {
                    continue;
                }
                let span = (network.max_delay_ns - network.min_delay_ns) as f64;
                let mut delay = network.min_delay_ns + (rng.unit() * span) as i64;
                if rng.unit() < network.very_late {
                    delay += 200_000_000;
                }
                if rng.unit() < network.duplicate {
                    in_flight.push((sent_local + delay + 3_000_000, frame.clone()));
                }
                in_flight.push((sent_local + delay, frame));
            }
            in_flight.sort_by_key(|(arrival, _)| *arrival);
            let due = in_flight.partition_point(|(arrival, _)| *arrival <= local_now);
            for (_, frame) in in_flight.drain(..due) {
                playout.push(&frame);
            }
            run.max_buffered_ns = run.max_buffered_ns.max(playout.buffered_ns());
            let mut out = vec![0.0; BLOCK * channels];
            playout.render(&mapping, local_now, &mut out);
            run.errors_ns.push(playout.stats().presentation_error_ns);
            run.output.extend_from_slice(&out);
            run.times
                .extend((0..BLOCK).map(|i| local_now + i as i64 * block_ns / BLOCK as i64));
        }
        run.stats = *playout.stats();
        run
    }

    /// Least-squares fit of a tone at the source position each output sample
    /// should present: returns (presentation error in seconds, THD+N in dB).
    fn fit_tone(
        output: &[f32],
        times: &[i64],
        mapping: &LinearClockMapping,
        hz: f64,
    ) -> (f64, f64) {
        let thetas: Vec<f64> = times
            .iter()
            .map(|&t| {
                let media_s = (mapping.local_to_leader_ns(t) - LATENCY_NS) as f64 / 1e9;
                2.0 * PI * hz * media_s
            })
            .collect();
        // Normal equations of y ≈ a·sin θ + b·cos θ.
        let (mut ss, mut cc, mut sc, mut ys, mut yc) = (0.0, 0.0, 0.0, 0.0, 0.0);
        for (&y, &theta) in output.iter().zip(&thetas) {
            let (s, c, y) = (theta.sin(), theta.cos(), f64::from(y));
            ss += s * s;
            cc += c * c;
            sc += s * c;
            ys += y * s;
            yc += y * c;
        }
        let det = ss * cc - sc.powi(2);
        let (a, b) = ((ys * cc - yc * sc) / det, (yc * ss - ys * sc) / det);
        let n = output.len() as f64;
        let amplitude = a.hypot(b);
        let phase = b.atan2(a);
        let residual: f64 = output
            .iter()
            .zip(&thetas)
            .map(|(&y, &theta)| {
                let fitted = a * theta.sin() + b * theta.cos();
                (f64::from(y) - fitted).powi(2)
            })
            .sum::<f64>()
            / n;
        let thd_db = 10.0 * (residual / (amplitude * amplitude / 2.0)).log10();
        (phase / (2.0 * PI * hz), thd_db)
    }

    fn tail(run: &Run, channels: usize, seconds: f64) -> (Vec<f32>, Vec<i64>) {
        let keep = (seconds * f64::from(RATE)) as usize;
        let frames = run.times.len();
        let first = frames - keep;
        let output = run.output[first * channels..]
            .iter()
            .copied()
            .step_by(channels)
            .collect();
        (output, run.times[first..].to_vec())
    }

    #[test]
    fn follows_a_skewed_leader_through_jitter_and_reordering() {
        for skew_ppm in [300.0, -300.0] {
            let mapping = LinearClockMapping {
                offset_ns: 1_234_567_891,
                skew_ppm,
            };
            let run = simulate(
                ChannelRole::Left,
                &mut |_| mapping,
                CLEAN_JITTER,
                0.0,
                6.0,
                (250.0, 440.0),
            );
            assert_eq!(run.stats.late, 0, "{skew_ppm} ppm: {:?}", run.stats);
            assert!(run.stats.duplicates > 0);
            assert_eq!(run.stats.skips + run.stats.inserts, 0);
            assert!(
                run.max_buffered_ns < 2 * LATENCY_NS,
                "fill {} ns",
                run.max_buffered_ns
            );
            let converged = &run.errors_ns[run.errors_ns.len() / 2..];
            assert!(
                converged.iter().all(|e| e.abs() < 1_000_000),
                "{skew_ppm} ppm: {:?}",
                converged.iter().max_by_key(|e| e.abs())
            );
            let (output, times) = tail(&run, 1, 2.0);
            let (error_s, thd_db) = fit_tone(&output, &times, &mapping, 250.0);
            assert!(error_s.abs() < 0.001, "{skew_ppm} ppm: error {error_s} s");
            assert!(thd_db < -90.0, "{skew_ppm} ppm: THD+N {thd_db} dB");
            let step_limit = 0.5 * 2.0 * PI * 250.0 / f64::from(RATE) * 1.05;
            let max_step = output
                .windows(2)
                .map(|w| (f64::from(w[1]) - f64::from(w[0])).abs())
                .fold(0.0, f64::max);
            assert!(max_step < step_limit, "{max_step} >= {step_limit}");
        }
    }

    #[test]
    fn the_drift_loop_absorbs_an_output_device_running_off_its_clock() {
        let mapping = LinearClockMapping {
            offset_ns: -77_000_000,
            skew_ppm: -300.0,
        };
        let run = simulate(
            ChannelRole::Left,
            &mut |_| mapping,
            CLEAN_JITTER,
            150.0,
            8.0,
            (250.0, 440.0),
        );
        assert_eq!(run.stats.skips + run.stats.inserts, 0);
        // The leader's clock runs 300 ppm slow against the member's, and the
        // device consumes its samples 150 ppm faster than the member's clock.
        assert!(
            (run.stats.ratio - (1.0 + 300e-6 - 150e-6)).abs() < 50e-6,
            "{}",
            run.stats.ratio
        );
        let converged = &run.errors_ns[run.errors_ns.len() / 2..];
        assert!(converged.iter().all(|e| e.abs() < 200_000));
        let (output, times) = tail(&run, 1, 2.0);
        let (error_s, thd_db) = fit_tone(&output, &times, &mapping, 250.0);
        assert!(error_s.abs() < 0.001, "error {error_s} s");
        assert!(thd_db < -90.0, "THD+N {thd_db} dB");
    }

    #[test]
    fn loss_and_late_frames_never_stall_playout() {
        let mapping = LinearClockMapping {
            offset_ns: 42,
            skew_ppm: 300.0,
        };
        let network = Network {
            loss: 0.05,
            duplicate: 0.05,
            very_late: 0.03,
            ..CLEAN_JITTER
        };
        let run = simulate(
            ChannelRole::Stereo,
            &mut |_| mapping,
            network,
            0.0,
            5.0,
            (250.0, 440.0),
        );
        assert!(run.stats.late > 0);
        assert!(run.stats.concealed_samples > 0);
        assert_eq!(run.stats.skips + run.stats.inserts, 0);
        assert!(run.max_buffered_ns < 2 * LATENCY_NS);
        let converged = &run.errors_ns[run.errors_ns.len() / 2..];
        assert!(converged.iter().all(|e| e.abs() < 1_000_000));
        // Every second after start-up carries sound: losses are brief gaps.
        let second = RATE as usize * 2;
        for chunk in run.output[second..].chunks(second) {
            let energy: f32 = chunk.iter().map(|s| s * s).sum::<f32>() / chunk.len() as f32;
            assert!(energy > 0.1, "energy {energy}");
        }
        // Concealment fades: no step larger than a full-scale tone's slope
        // allows, even across gaps.
        let left: Vec<f32> = run.output[second..].iter().copied().step_by(2).collect();
        let max_step = left
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0, f32::max);
        assert!(max_step < 0.05, "step {max_step}");
    }

    #[test]
    fn a_clock_estimate_step_is_slewed_and_a_large_one_is_jumped() {
        let before = LinearClockMapping {
            offset_ns: 0,
            skew_ppm: 100.0,
        };
        let small = LinearClockMapping {
            offset_ns: 2_000_000,
            ..before
        };
        let run = simulate(
            ChannelRole::Mono,
            &mut |t| if t < 1.0 { before } else { small },
            CLEAN_JITTER,
            0.0,
            10.0,
            (250.0, 250.0),
        );
        assert_eq!(run.stats.skips + run.stats.inserts, 0);
        assert!(run.errors_ns.iter().any(|e| e.abs() > 1_500_000));
        assert!(
            run.errors_ns[run.errors_ns.len() - 100..]
                .iter()
                .all(|e| e.abs() < 100_000)
        );

        let large = LinearClockMapping {
            offset_ns: -50_000_000,
            ..before
        };
        let run = simulate(
            ChannelRole::Mono,
            &mut |t| if t < 1.0 { before } else { large },
            CLEAN_JITTER,
            0.0,
            3.0,
            (250.0, 250.0),
        );
        assert_eq!(run.stats.skips + run.stats.inserts, 1, "{:?}", run.stats);
        assert!(
            run.errors_ns[run.errors_ns.len() - 100..]
                .iter()
                .all(|e| e.abs() < 1_000_000)
        );
    }

    fn render_role(role: ChannelRole, left_hz: f64, right_hz: f64) -> Vec<f32> {
        let mapping = LinearClockMapping::default();
        let mut playout = SyncPlayout::new(PlayoutConfig::new(RATE, LATENCY_NS, role));
        let seconds = 1;
        let count = RATE as usize * seconds;
        playout.push(&AudioFrame {
            seq: 0,
            media_ts: 0,
            channels: 2,
            payload: Payload::F32(stereo_tone(0, count, left_hz, right_hz)),
        });
        let mut out = vec![0.0; count / 2 * role.channels()];
        playout.render(&mapping, LATENCY_NS + NS_PER_SEC / 4, &mut out);
        out
    }

    /// Amplitude of `hz` in a mono signal at the stream rate.
    fn amplitude(signal: &[f32], hz: f64) -> f64 {
        let (mut s, mut c) = (0.0, 0.0);
        for (i, &y) in signal.iter().enumerate() {
            let theta = 2.0 * PI * hz * i as f64 / f64::from(RATE);
            s += f64::from(y) * theta.sin();
            c += f64::from(y) * theta.cos();
        }
        2.0 * s.hypot(c) / signal.len() as f64
    }

    #[test]
    fn channel_roles_select_and_mix_the_stereo_source() {
        let left = render_role(ChannelRole::Left, 440.0, 660.0);
        assert!((amplitude(&left, 440.0) - 0.5).abs() < 0.01);
        assert!(amplitude(&left, 660.0) < 0.001);

        let right = render_role(ChannelRole::Right, 440.0, 660.0);
        assert!(amplitude(&right, 440.0) < 0.001);
        assert!((amplitude(&right, 660.0) - 0.5).abs() < 0.01);

        for role in [ChannelRole::Mono, ChannelRole::Center] {
            let mid = render_role(role, 440.0, 660.0);
            assert!((amplitude(&mid, 440.0) - 0.25).abs() < 0.01);
            assert!((amplitude(&mid, 660.0) - 0.25).abs() < 0.01);
        }

        let stereo = render_role(ChannelRole::Stereo, 440.0, 660.0);
        let l: Vec<f32> = stereo.iter().copied().step_by(2).collect();
        let r: Vec<f32> = stereo.iter().copied().skip(1).step_by(2).collect();
        assert!((amplitude(&l, 440.0) - 0.5).abs() < 0.01);
        assert!(amplitude(&l, 660.0) < 0.001);
        assert!((amplitude(&r, 660.0) - 0.5).abs() < 0.01);

        // The LFE feed keeps the bass and removes the rest.
        let lfe = render_role(ChannelRole::Lfe, 40.0, 2_000.0);
        let settled = &lfe[RATE as usize / 8..];
        assert!(amplitude(settled, 40.0) > 0.24);
        assert!(20.0 * (amplitude(settled, 2_000.0) / 0.25).log10() < -60.0);
    }

    #[test]
    fn push_classifies_duplicates_late_and_malformed_frames() {
        let mapping = LinearClockMapping::default();
        let mut playout = SyncPlayout::new(PlayoutConfig::new(RATE, LATENCY_NS, ChannelRole::Mono));
        let frame = |seq: u64, media_ts: i64| AudioFrame {
            seq,
            media_ts,
            channels: 1,
            payload: Payload::I16(vec![1_000; FRAME]),
        };
        assert_eq!(playout.push(&frame(1, 4_800)), PushOutcome::Queued);
        assert_eq!(playout.push(&frame(1, 4_800)), PushOutcome::Duplicate);
        assert_eq!(
            playout.push(&AudioFrame {
                channels: 2,
                payload: Payload::I16(vec![0; 3]),
                ..frame(2, 0)
            }),
            PushOutcome::Malformed
        );
        let mut out = vec![0.0; BLOCK];
        // Playout at 0.2 s: sample 9 600 is due, so a frame at 0 is late.
        playout.render(&mapping, LATENCY_NS + NS_PER_SEC / 5, &mut out);
        assert_eq!(playout.push(&frame(3, 0)), PushOutcome::Late);
        assert_eq!(playout.push(&frame(4, 9_600 + 2_400)), PushOutcome::Queued);
        let stats = playout.stats();
        assert_eq!(
            (stats.queued, stats.duplicates, stats.late, stats.malformed),
            (2, 1, 1, 1)
        );
        assert_eq!(stats.last_seq, Some(4));
    }

    #[test]
    fn capacity_discards_the_oldest_frames() {
        let mut config = PlayoutConfig::new(RATE, LATENCY_NS, ChannelRole::Mono);
        config.capacity_ns = 100_000_000;
        let mut playout = SyncPlayout::new(config);
        for i in 0..100 {
            playout.push(&AudioFrame {
                seq: i,
                media_ts: i as i64 * FRAME as i64,
                channels: 1,
                payload: Payload::F32(vec![0.0; FRAME]),
            });
        }
        assert!(playout.buffered_ns() <= 100_000_000);
        assert!(playout.stats().overflowed > 0);
    }

    #[test]
    fn linear_mapping_round_trips() {
        let mapping = LinearClockMapping {
            offset_ns: -3_000_000_123,
            skew_ppm: 287.5,
        };
        for leader in [0, 1, 123_456_789_012, 9_000_000_000_000_000] {
            let local = mapping.leader_to_local_ns(leader);
            assert!((mapping.local_to_leader_ns(local) - leader).abs() <= 1);
        }
    }
}
