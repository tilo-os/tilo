//! Bounded local voice processing. These inert values carry no call authority
//! or microphone permission; the privileged service owns both.
use std::{
    collections::{BTreeMap, VecDeque},
    time::{Duration, Instant},
};

#[cfg(all(feature = "voice", unix, not(target_arch = "wasm32")))]
pub mod gst;

pub const RATE: u32 = 48_000;
pub const FRAME_SAMPLES: usize = 960;
pub const FRAME_DURATION: Duration = Duration::from_millis(20);
pub const MAX_PACKET_BYTES: usize = 4064;
/// Voices this device plays at once: its decode budget, not a limit on a
/// call. A call has any number of participants; with DTX a silent one sends a
/// comfort-noise packet every 400 ms, so the voices that hold a slot are the
/// ones speaking. A new voice takes the slot of the one quiet longest, and is
/// not heard while every slot is speaking.
pub const MAX_PUBLISHERS: usize = 8;
pub const SEND_LIFETIME: Duration = Duration::from_millis(100);
pub const RECEIVE_LIFETIME: Duration = Duration::from_millis(200);

#[derive(Default)]
pub struct CaptureTimeline {
    epoch: Option<Instant>,
    source: Option<(u64, u64)>,
    last: Option<u64>,
}
impl CaptureTimeline {
    pub fn reset_source(&mut self) {
        self.source = None;
    }
    /// Map a capture source's raw sample clock onto the monotonic 48 kHz media timeline.
    ///
    /// # Errors
    ///
    /// [`AudioError::InvalidFrame`] for a non-frame-aligned, regressing or overflowing timestamp.
    pub fn timestamp(&mut self, raw: u64, captured_at: Instant) -> Result<u64, AudioError> {
        let (raw_base, media_base) = if let Some(source) = self.source {
            source
        } else {
            let source = (raw, self.minimum_timestamp(captured_at)?);
            self.source = Some(source);
            source
        };
        let delta = raw.checked_sub(raw_base).ok_or(AudioError::InvalidFrame)?;
        if delta % 960 != 0 {
            return Err(AudioError::InvalidFrame);
        }
        let timestamp = media_base
            .checked_add(delta)
            .ok_or(AudioError::InvalidFrame)?;
        if self.last.is_some_and(|last| timestamp <= last) {
            return Err(AudioError::InvalidFrame);
        }
        self.last = Some(timestamp);
        Ok(timestamp)
    }
    /// Stamp a locally encoded complete 20ms frame on the wire sample clock.
    /// Device PTS is independently checked for freshness by the caller; its
    /// sub-frame jitter must not change the encoded frame's sample duration.
    /// Capture gaps and restarts retain the original monotonic epoch.
    ///
    /// # Errors
    ///
    /// [`AudioError::InvalidFrame`] if the wire timestamp would overflow.
    pub fn frame_timestamp(&mut self, captured_at: Instant) -> Result<u64, AudioError> {
        let timestamp = self.minimum_timestamp(captured_at)?;
        self.source = None;
        self.last = Some(timestamp);
        Ok(timestamp)
    }
    fn minimum_timestamp(&mut self, captured_at: Instant) -> Result<u64, AudioError> {
        let epoch = *self.epoch.get_or_insert(captured_at);
        let elapsed_frames =
            u64::try_from(captured_at.saturating_duration_since(epoch).as_millis() / 20)
                .map_err(|_| AudioError::InvalidFrame)?;
        let elapsed = elapsed_frames
            .checked_mul(960)
            .ok_or(AudioError::InvalidFrame)?;
        let next = self
            .last
            .map(|last| last.checked_add(960).ok_or(AudioError::InvalidFrame))
            .transpose()?
            .unwrap_or(0);
        Ok(elapsed.max(next))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct PublicationId {
    pub namespace: String,
    pub carrier_generation: u64,
    /// Which announcement of the namespace on this carrier: a publisher that
    /// leaves and comes back restarts its sequence, so its return is a new
    /// publication with fresh playout and decoder state, while every other
    /// voice on the carrier plays on.
    pub incarnation: u64,
}

impl PublicationId {
    /// Carrier generation, then incarnation: what supersedes what.
    fn epoch(&self) -> (u64, u64) {
        (self.carrier_generation, self.incarnation)
    }
}

/// Contiguous frames that make a voice heard: speech arrives every 20 ms,
/// while a silent (DTX) publisher's comfort-noise updates come 400 ms apart.
const HEARD_RUN: u32 = 3;
/// A voice is heard while its last contiguous frame is this recent.
const HEARD_FOR: Duration = Duration::from_millis(120);

#[derive(Clone, Debug)]
pub struct CapturedOpus {
    pub timestamp_48k: u64,
    pub duration_samples: u16,
    pub payload: Vec<u8>,
    pub captured_at: Instant,
}

#[derive(Clone, Debug)]
pub struct ReceivedOpus {
    pub publisher: PublicationId,
    pub sequence: u64,
    pub timestamp_48k: u64,
    pub duration_samples: u16,
    pub payload: Vec<u8>,
    pub received_at: Instant,
    /// Original transport deadline; processing never starts a new lifetime.
    pub expires_at: Instant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioError {
    Unavailable,
    InvalidFrame,
    Capacity,
    Revoked,
    DeviceLost,
    Codec,
    Closed,
}

impl std::fmt::Display for AudioError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "voice audio: {self:?}")
    }
}
impl std::error::Error for AudioError {}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AudioStats {
    pub late: u64,
    pub duplicates: u64,
    pub lost: u64,
    pub overruns: u64,
}

#[derive(Default)]
pub struct CaptureQueue {
    active: bool,
    frames: VecDeque<CapturedOpus>,
}
impl CaptureQueue {
    pub fn start(&mut self) {
        self.stop();
        self.active = true;
    }
    pub fn stop(&mut self) {
        self.active = false;
        self.frames.clear();
    }
    /// Queue one captured frame, dropping frames past their send lifetime.
    ///
    /// # Errors
    ///
    /// [`AudioError::Revoked`] while stopped and [`AudioError::InvalidFrame`] for a malformed or
    /// future-dated frame.
    pub fn push(&mut self, frame: CapturedOpus, now: Instant) -> Result<(), AudioError> {
        if !self.active {
            return Err(AudioError::Revoked);
        }
        validate_frame(frame.duration_samples, &frame.payload)?;
        if frame.captured_at > now {
            return Err(AudioError::InvalidFrame);
        }
        self.frames
            .retain(|f| now.duration_since(f.captured_at) < SEND_LIFETIME);
        if now.duration_since(frame.captured_at) >= SEND_LIFETIME {
            return Ok(());
        }
        if self.frames.len() == 5 {
            self.frames.pop_front();
        }
        self.frames.push_back(frame);
        Ok(())
    }
    pub fn drain(&mut self, now: Instant) -> Vec<CapturedOpus> {
        self.frames
            .drain(..)
            .filter(|f| now.saturating_duration_since(f.captured_at) < SEND_LIFETIME)
            .collect()
    }
}

/// Whether an encoded Opus packet carries anything. With DTX the encoder
/// returns a packet of at most two bytes for a silent frame that needs no
/// transmission (libopus `opus_encode`); the receiver's decoder fills the gap
/// with comfort noise from the last update it got.
#[must_use]
pub fn needs_transmission(packet: &[u8]) -> bool {
    packet.len() > 2
}

fn validate_frame(duration: u16, payload: &[u8]) -> Result<(), AudioError> {
    if duration != 960 || payload.is_empty() || payload.len() > MAX_PACKET_BYTES {
        return Err(AudioError::InvalidFrame);
    }
    Ok(())
}

pub struct PlayoutFrame {
    pub publisher: PublicationId,
    pub sequence: u64,
    pub timestamp_48k: u64,
    pub expires_at: Instant,
    /// `None` requests exactly one frame of codec loss concealment.
    pub payload: Option<Vec<u8>>,
}

#[derive(Default)]
pub struct JitterBuffer {
    publishers: BTreeMap<String, Publisher>,
    stats: AudioStats,
}

struct Publisher {
    id: PublicationId,
    frames: BTreeMap<u64, ReceivedOpus>,
    base_timestamp: u64,
    next_sequence: u64,
    next_timestamp: u64,
    next_play: Instant,
    last_receive: Instant,
    last_timestamp: u64,
    active_until: Instant,
    jitter_ms: f64,
    target_ms: u64,
    applied_target_ms: u64,
    /// Contiguous frames up to the last one received.
    run: u32,
}

impl Publisher {
    /// A publisher heard for the first time, with `frame`.
    fn first(frame: &ReceivedOpus) -> Self {
        Self {
            id: frame.publisher.clone(),
            frames: BTreeMap::new(),
            base_timestamp: frame.timestamp_48k,
            next_sequence: frame.sequence,
            next_timestamp: frame.timestamp_48k,
            next_play: frame
                .received_at
                .checked_add(Duration::from_millis(60))
                .unwrap_or(frame.received_at),
            last_receive: frame.received_at,
            last_timestamp: frame.timestamp_48k,
            active_until: frame.expires_at,
            jitter_ms: 0.0,
            target_ms: 60,
            applied_target_ms: 60,
            run: 0,
        }
    }

    /// Count a frame at `timestamp_48k` into the run of contiguous frames:
    /// speech runs on, a silent publisher's comfort noise starts over.
    fn count_run(&mut self, timestamp_48k: u64) {
        self.run = if self.run > 0 && timestamp_48k == self.last_timestamp.saturating_add(960) {
            self.run.saturating_add(1)
        } else {
            1
        };
    }

    /// Update the arrival-jitter estimate and playout target from one frame
    /// that is newer in both media time and arrival time.
    fn observe_arrival(&mut self, timestamp_48k: u64, received_at: Instant) {
        if timestamp_48k <= self.last_timestamp || received_at < self.last_receive {
            return;
        }
        // A gap beyond u32 samples (~25 hours) saturates the clamped target anyway.
        let media_samples =
            u32::try_from(timestamp_48k.saturating_sub(self.last_timestamp)).unwrap_or(u32::MAX);
        let media_ms = f64::from(media_samples) / 48.0;
        let arrival_ms = received_at.duration_since(self.last_receive).as_secs_f64() * 1000.0;
        self.jitter_ms += ((arrival_ms - media_ms).abs() - self.jitter_ms) / 16.0;
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "the rounded target is clamped to 40..=120 ms before the cast"
        )]
        let target_ms = (40.0 + self.jitter_ms * 4.0).round().clamp(40.0, 120.0) as u64;
        self.target_ms = target_ms;
        self.last_timestamp = timestamp_48k;
        self.last_receive = received_at;
    }
}

/// A collection length as a saturating statistics count.
fn count(len: usize) -> u64 {
    u64::try_from(len).unwrap_or(u64::MAX)
}

impl JitterBuffer {
    /// Admit one received frame into its publisher's playout queue.
    ///
    /// # Errors
    ///
    /// [`AudioError::InvalidFrame`] for a malformed or inconsistent frame, [`AudioError::Revoked`]
    /// for a superseded carrier generation, and [`AudioError::Capacity`] when full.
    pub fn push(&mut self, mut frame: ReceivedOpus, now: Instant) -> Result<(), AudioError> {
        validate_frame(frame.duration_samples, &frame.payload)?;
        if frame.publisher.namespace.is_empty()
            || frame.publisher.namespace.len() > 256
            || frame.publisher.namespace.chars().any(char::is_control)
            || frame.received_at > now
            || frame.timestamp_48k.checked_add(960).is_none()
            || frame.sequence == u64::MAX
        {
            return Err(AudioError::InvalidFrame);
        }
        if let Some(lifetime) = frame.received_at.checked_add(RECEIVE_LIFETIME) {
            frame.expires_at = frame.expires_at.min(lifetime);
        }
        if frame.expires_at <= now {
            self.stats.late = self.stats.late.saturating_add(1);
            return Ok(());
        }
        if !self.admit_publisher(&frame, now)? {
            return Ok(());
        }
        let name = frame.publisher.namespace.clone();
        let publisher = self
            .publishers
            .get_mut(&name)
            .ok_or(AudioError::InvalidFrame)?;
        if frame.timestamp_48k.abs_diff(publisher.base_timestamp) % 960 != 0 {
            return Err(AudioError::InvalidFrame);
        }
        if frame.sequence < publisher.next_sequence
            || publisher
                .frames
                .values()
                .any(|f| f.sequence == frame.sequence)
        {
            self.stats.duplicates = self.stats.duplicates.saturating_add(1);
            return Ok(());
        }
        if frame.timestamp_48k < publisher.next_timestamp {
            self.stats.late = self.stats.late.saturating_add(1);
            return Ok(());
        }
        if publisher
            .frames
            .values()
            .any(|f| f.sequence.cmp(&frame.sequence) != f.timestamp_48k.cmp(&frame.timestamp_48k))
        {
            return Err(AudioError::InvalidFrame);
        }
        let before = publisher.frames.len();
        publisher.frames.retain(|_, f| f.expires_at > now);
        self.stats.late = self
            .stats
            .late
            .saturating_add(count(before.saturating_sub(publisher.frames.len())));
        if publisher.frames.len() == 10 {
            self.stats.overruns = self.stats.overruns.saturating_add(1);
            return Err(AudioError::Capacity);
        }
        // A delayed callback never creates a fresh 200ms playback window.
        if publisher.active_until <= now {
            publisher.next_sequence = frame.sequence;
            publisher.next_timestamp = frame.timestamp_48k;
            publisher.next_play = frame
                .received_at
                .checked_add(Duration::from_millis(publisher.target_ms))
                .unwrap_or(frame.received_at);
        }
        publisher.count_run(frame.timestamp_48k);
        publisher.observe_arrival(frame.timestamp_48k, frame.received_at);
        publisher.active_until = publisher.active_until.max(frame.expires_at);
        publisher.frames.insert(frame.timestamp_48k, frame);
        Ok(())
    }

    /// Find or make `frame`'s publisher slot: a newer carrier generation or
    /// incarnation replaces the old slot, and a new voice takes the quietest idle slot
    /// when all are taken. `false` when every slot is speaking, so this
    /// voice waits for one.
    fn admit_publisher(&mut self, frame: &ReceivedOpus, now: Instant) -> Result<bool, AudioError> {
        let name = &frame.publisher.namespace;
        if let Some(existing) = self.publishers.get(name) {
            if frame.publisher.epoch() < existing.id.epoch() {
                return Err(AudioError::Revoked);
            }
            if frame.publisher.epoch() > existing.id.epoch() {
                self.publishers.remove(name);
            }
        }
        if !self.publishers.contains_key(name) {
            if self.publishers.len() >= MAX_PUBLISHERS {
                let quiet = self
                    .publishers
                    .iter()
                    .filter(|(_, publisher)| {
                        publisher.active_until <= now && publisher.frames.is_empty()
                    })
                    .min_by_key(|(_, publisher)| publisher.last_receive)
                    .map(|(idle, _)| idle.clone());
                let Some(quiet) = quiet else {
                    // Every slot is speaking: this voice waits for one.
                    self.stats.overruns = self.stats.overruns.saturating_add(1);
                    return Ok(false);
                };
                self.publishers.remove(&quiet);
            }
            self.publishers
                .insert(name.clone(), Publisher::first(frame));
        }
        Ok(true)
    }

    pub fn poll(&mut self, now: Instant) -> Vec<PlayoutFrame> {
        let mut out = Vec::with_capacity(self.publishers.len());
        for publisher in self.publishers.values_mut() {
            if now >= publisher.active_until {
                self.stats.late = self
                    .stats
                    .late
                    .saturating_add(count(publisher.frames.len()));
                publisher.frames.clear();
                continue;
            }
            if now < publisher.next_play {
                continue;
            }
            // Never burst a backlog after a stalled audio/event thread.
            let skipped = now.duration_since(publisher.next_play).as_millis() / 20;
            let skipped = u64::try_from(skipped).unwrap_or(u64::MAX);
            let Some(timestamp) = skipped
                .checked_mul(960)
                .and_then(|n| publisher.next_timestamp.checked_add(n))
            else {
                continue;
            };
            publisher.frames.retain(|ts, _| *ts >= timestamp);
            self.stats.late = self.stats.late.saturating_add(skipped);
            // PLC has no new media lifetime: it is bounded by the last
            // authenticated receive window, including asynchronous decode.
            let mut deadline = publisher.active_until;
            let mut sequence = publisher.next_sequence;
            let payload = publisher.frames.remove(&timestamp).and_then(|frame| {
                sequence = frame.sequence;
                publisher.next_sequence = sequence.saturating_add(1);
                if frame.expires_at <= now {
                    self.stats.late = self.stats.late.saturating_add(1);
                    None
                } else {
                    deadline = frame.expires_at;
                    Some(frame.payload)
                }
            });
            if payload.is_none() {
                self.stats.lost = self.stats.lost.saturating_add(1);
            }
            out.push(PlayoutFrame {
                publisher: publisher.id.clone(),
                sequence,
                timestamp_48k: timestamp,
                expires_at: deadline,
                payload,
            });
            publisher.next_timestamp = timestamp.saturating_add(960);
            let adjustment = publisher.target_ms.cmp(&publisher.applied_target_ms);
            let interval_ms = match adjustment {
                std::cmp::Ordering::Greater => {
                    publisher.applied_target_ms = publisher.applied_target_ms.saturating_add(1);
                    21
                }
                std::cmp::Ordering::Less => {
                    publisher.applied_target_ms = publisher.applied_target_ms.saturating_sub(1);
                    19
                }
                std::cmp::Ordering::Equal => 20,
            };
            let advance =
                Duration::from_millis(skipped.saturating_mul(20).saturating_add(interval_ms));
            if let Some(next_play) = publisher.next_play.checked_add(advance) {
                publisher.next_play = next_play;
            }
        }
        out
    }
    /// The publishers heard talking at `now`: frames arriving back to back
    /// within the last [`HEARD_FOR`], not a silent publisher's comfort noise.
    pub fn heard(&self, now: Instant) -> impl Iterator<Item = &PublicationId> {
        self.publishers
            .values()
            .filter(move |publisher| {
                publisher.run >= HEARD_RUN
                    && now.saturating_duration_since(publisher.last_receive) < HEARD_FOR
            })
            .map(|publisher| &publisher.id)
    }
    /// Whether `publisher` holds one of the voices this device plays.
    pub fn contains(&self, publisher: &PublicationId) -> bool {
        self.publishers
            .get(&publisher.namespace)
            .is_some_and(|p| p.id == *publisher)
    }
    pub fn remove(&mut self, publisher: &PublicationId) {
        if self
            .publishers
            .get(&publisher.namespace)
            .is_some_and(|p| p.id == *publisher)
        {
            self.publishers.remove(&publisher.namespace);
        }
    }
    pub fn clear(&mut self) {
        self.publishers.clear();
    }
    pub fn stats(&self) -> AudioStats {
        self.stats
    }
    pub fn queued(&self) -> usize {
        self.publishers.values().map(|p| p.frames.len()).sum()
    }
    pub fn queued_for(&self, publisher: &PublicationId) -> usize {
        self.publishers
            .get(&publisher.namespace)
            .filter(|p| p.id == *publisher)
            .map_or(0, |p| p.frames.len())
    }
    pub fn target_ms(&self, publisher: &PublicationId) -> Option<u64> {
        self.publishers
            .get(&publisher.namespace)
            .filter(|p| p.id == *publisher)
            .map(|p| p.target_ms)
    }
}

/// Mix one simultaneous 20ms frame per publisher, saturating only after summing.
///
/// # Errors
///
/// [`AudioError::Capacity`] for more than [`MAX_PUBLISHERS`] frames and
/// [`AudioError::InvalidFrame`] for a frame that is not [`FRAME_SAMPLES`] long.
pub fn mix_frames(frames: &[&[i16]]) -> Result<Vec<i16>, AudioError> {
    if frames.len() > MAX_PUBLISHERS {
        return Err(AudioError::Capacity);
    }
    if frames.iter().any(|f| f.len() != FRAME_SAMPLES) {
        return Err(AudioError::InvalidFrame);
    }
    let mut sums = vec![0i32; FRAME_SAMPLES];
    for frame in frames {
        for (sum, sample) in sums.iter_mut().zip(*frame) {
            // At most eight i16 samples: the i32 sum cannot saturate.
            *sum = sum.saturating_add(i32::from(*sample));
        }
    }
    Ok(sums
        .into_iter()
        .map(|s| i16::try_from(s).unwrap_or(if s < 0 { i16::MIN } else { i16::MAX }))
        .collect())
}

#[cfg(test)]
mod tests;
