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
    pub fn timestamp(&mut self, raw: u64, captured_at: Instant) -> Result<u64, AudioError> {
        let epoch = *self.epoch.get_or_insert(captured_at);
        let (raw_base, media_base) = match self.source {
            Some(source) => source,
            None => {
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
                let source = (raw, elapsed.max(next));
                self.source = Some(source);
                source
            }
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
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct PublicationId {
    pub namespace: String,
    pub carrier_generation: u64,
}

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
}

impl JitterBuffer {
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
        frame.expires_at = frame.expires_at.min(frame.received_at + RECEIVE_LIFETIME);
        if frame.expires_at <= now {
            self.stats.late += 1;
            return Ok(());
        }
        let name = frame.publisher.namespace.clone();
        if let Some(existing) = self.publishers.get(&name) {
            if frame.publisher.carrier_generation < existing.id.carrier_generation {
                return Err(AudioError::Revoked);
            }
            if frame.publisher.carrier_generation > existing.id.carrier_generation {
                self.publishers.remove(&name);
            }
        }
        if !self.publishers.contains_key(&name) {
            if self.publishers.len() == MAX_PUBLISHERS {
                return Err(AudioError::Capacity);
            }
            self.publishers.insert(
                name.clone(),
                Publisher {
                    id: frame.publisher.clone(),
                    frames: BTreeMap::new(),
                    base_timestamp: frame.timestamp_48k,
                    next_sequence: frame.sequence,
                    next_timestamp: frame.timestamp_48k,
                    next_play: frame.received_at + Duration::from_millis(60),
                    last_receive: frame.received_at,
                    last_timestamp: frame.timestamp_48k,
                    active_until: frame.expires_at,
                    jitter_ms: 0.0,
                    target_ms: 60,
                    applied_target_ms: 60,
                },
            );
        }
        let publisher = self
            .publishers
            .get_mut(&name)
            .ok_or(AudioError::InvalidFrame)?;
        let timestamp_delta =
            i128::from(frame.timestamp_48k) - i128::from(publisher.base_timestamp);
        if timestamp_delta % 960 != 0 {
            return Err(AudioError::InvalidFrame);
        }
        if frame.sequence < publisher.next_sequence
            || publisher
                .frames
                .values()
                .any(|f| f.sequence == frame.sequence)
        {
            self.stats.duplicates += 1;
            return Ok(());
        }
        if frame.timestamp_48k < publisher.next_timestamp {
            self.stats.late += 1;
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
        self.stats.late += (before - publisher.frames.len()) as u64;
        if publisher.frames.len() == 10 {
            self.stats.overruns += 1;
            return Err(AudioError::Capacity);
        }
        // A delayed callback never creates a fresh 200ms playback window.
        if publisher.active_until <= now {
            publisher.next_sequence = frame.sequence;
            publisher.next_timestamp = frame.timestamp_48k;
            publisher.next_play = frame.received_at + Duration::from_millis(publisher.target_ms);
        }
        if frame.timestamp_48k > publisher.last_timestamp
            && frame.received_at >= publisher.last_receive
        {
            let media_ms = (frame.timestamp_48k - publisher.last_timestamp) as f64 / 48.0;
            let arrival_ms = frame
                .received_at
                .duration_since(publisher.last_receive)
                .as_secs_f64()
                * 1000.0;
            publisher.jitter_ms += ((arrival_ms - media_ms).abs() - publisher.jitter_ms) / 16.0;
            publisher.target_ms = (40.0 + publisher.jitter_ms * 4.0)
                .round()
                .clamp(40.0, 120.0) as u64;
            publisher.last_timestamp = frame.timestamp_48k;
            publisher.last_receive = frame.received_at;
        }
        publisher.active_until = publisher.active_until.max(frame.expires_at);
        publisher.frames.insert(frame.timestamp_48k, frame);
        Ok(())
    }

    pub fn poll(&mut self, now: Instant) -> Vec<PlayoutFrame> {
        let mut out = Vec::with_capacity(self.publishers.len());
        for publisher in self.publishers.values_mut() {
            if now >= publisher.active_until {
                self.stats.late += publisher.frames.len() as u64;
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
                    self.stats.late += 1;
                    None
                } else {
                    deadline = frame.expires_at;
                    Some(frame.payload)
                }
            });
            if payload.is_none() {
                self.stats.lost += 1;
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
                    publisher.applied_target_ms += 1;
                    21
                }
                std::cmp::Ordering::Less => {
                    publisher.applied_target_ms -= 1;
                    19
                }
                std::cmp::Ordering::Equal => 20,
            };
            publisher.next_play += Duration::from_millis(skipped.saturating_mul(20) + interval_ms);
        }
        out
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
            *sum += i32::from(*sample);
        }
    }
    Ok(sums
        .into_iter()
        .map(|s| s.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16)
        .collect())
}

#[cfg(test)]
mod tests;
