//! The transport-independent tilo-voice/1 object profile.

pub const HEADER_BYTES: usize = 32;
/// The header of an object whose voice is a bridged person's: the first one
/// plus their key.
pub const BRIDGED_HEADER_BYTES: usize = 64;
pub const MAX_OBJECT_BYTES: usize = 4096;
pub const FRAME_SAMPLES: u16 = 960;
pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: u8 = 1;
pub const FRAMES_PER_GROUP: u64 = 5;
/// A datagram bundle carries the newest object and up to two earlier ones.
pub const MAX_BUNDLE_OBJECTS: usize = 3;
/// Bytes a sender puts in one datagram bundle (the browser's budget), below
/// any QUIC path MTU. A bundle received larger is forwarded as it came.
pub const MAX_BUNDLE_BYTES: usize = 1000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VoiceObject {
    pub sequence: u64,
    pub timestamp: u64,
    pub duration: u16,
    /// The bridged person whose voice this is, when a bridge carries them: the
    /// key their participant is known by, never the bridge device's. `None`
    /// for the voice of the device that sends it.
    pub speaker: Option<[u8; 32]>,
    pub payload: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("invalid tilo-voice/1 object")]
pub struct InvalidVoiceObject;

/// Read the fixed-width header field at `at`, refusing a short buffer.
fn field<const N: usize>(bytes: &[u8], at: usize) -> Result<[u8; N], InvalidVoiceObject> {
    let end = at.checked_add(N).ok_or(InvalidVoiceObject)?;
    bytes
        .get(at..end)
        .and_then(|field| field.try_into().ok())
        .ok_or(InvalidVoiceObject)
}

impl VoiceObject {
    /// Encode this object in the tilo-voice/1 wire layout: an object that names
    /// a bridged speaker carries their key in its header.
    pub fn encode(&self) -> Result<Vec<u8>, InvalidVoiceObject> {
        let header = if self.speaker.is_some() {
            BRIDGED_HEADER_BYTES
        } else {
            HEADER_BYTES
        };
        if self.duration != FRAME_SAMPLES
            || self.payload.is_empty()
            || self.payload.len() > MAX_OBJECT_BYTES.saturating_sub(header)
        {
            return Err(InvalidVoiceObject);
        }
        let length = u32::try_from(self.payload.len()).map_err(|_| InvalidVoiceObject)?;
        let mut bytes = Vec::with_capacity(header.saturating_add(self.payload.len()));
        let version = if self.speaker.is_some() { 2 } else { 1 };
        bytes.extend_from_slice(&[version, 0, 0, 0]);
        bytes.extend_from_slice(&self.sequence.to_le_bytes());
        bytes.extend_from_slice(&self.timestamp.to_le_bytes());
        bytes.extend_from_slice(&self.duration.to_le_bytes());
        bytes.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
        bytes.extend_from_slice(&[CHANNELS, 0]);
        if let Some(speaker) = &self.speaker {
            bytes.extend_from_slice(speaker);
        }
        bytes.extend_from_slice(&length.to_le_bytes());
        debug_assert_eq!(bytes.len(), header);
        bytes.extend_from_slice(&self.payload);
        Ok(bytes)
    }

    /// Decode one complete, valid tilo-voice/1 object.
    pub fn decode(bytes: &[u8]) -> Result<Self, InvalidVoiceObject> {
        let header = validate(bytes)?;
        let speaker = if header == BRIDGED_HEADER_BYTES {
            Some(field::<32>(bytes, 28)?)
        } else {
            None
        };
        Ok(Self {
            sequence: u64::from_le_bytes(field(bytes, 4)?),
            timestamp: u64::from_le_bytes(field(bytes, 12)?),
            duration: FRAME_SAMPLES,
            speaker,
            payload: bytes.get(header..).ok_or(InvalidVoiceObject)?.to_vec(),
        })
    }
}

/// The header size of the object `bytes` begins with: whose voice it carries
/// decides how long the header is.
fn header_size(bytes: &[u8]) -> Result<usize, InvalidVoiceObject> {
    match bytes.first() {
        Some(1) => Ok(HEADER_BYTES),
        Some(2) => Ok(BRIDGED_HEADER_BYTES),
        _ => Err(InvalidVoiceObject),
    }
}

/// Validate without allocating or copying the encoded payload. Used at the
/// relay's complete-frame boundary before subscribers can observe an object.
/// The header size it read.
pub fn validate(bytes: &[u8]) -> Result<usize, InvalidVoiceObject> {
    let header = header_size(bytes)?;
    if !(header.saturating_add(1)..=MAX_OBJECT_BYTES).contains(&bytes.len()) {
        return Err(InvalidVoiceObject);
    }
    let payload_length =
        u32::try_from(bytes.len().saturating_sub(header)).map_err(|_| InvalidVoiceObject)?;
    let length_at = header.checked_sub(4).ok_or(InvalidVoiceObject)?;
    let version = if header == BRIDGED_HEADER_BYTES { 2 } else { 1 };
    if field::<4>(bytes, 0)? != [version, 0, 0, 0]
        || field::<2>(bytes, 20)? != FRAME_SAMPLES.to_le_bytes()
        || field::<4>(bytes, 22)? != SAMPLE_RATE.to_le_bytes()
        || field::<2>(bytes, 26)? != [CHANNELS, 0]
        || field::<4>(bytes, length_at)? != payload_length.to_le_bytes()
    {
        return Err(InvalidVoiceObject);
    }
    Ok(header)
}

/// Split a datagram bundle (one to [`MAX_BUNDLE_OBJECTS`] complete, individually
/// valid objects back to back, newest first) without copying. Stream frames are
/// always single objects, which are one-object bundles.
pub fn bundle_objects(bytes: &[u8]) -> Result<Vec<&[u8]>, InvalidVoiceObject> {
    if bytes.is_empty() || bytes.len() > MAX_OBJECT_BYTES {
        return Err(InvalidVoiceObject);
    }
    let mut objects = Vec::with_capacity(MAX_BUNDLE_OBJECTS);
    let mut rest = bytes;
    while !rest.is_empty() {
        if objects.len() == MAX_BUNDLE_OBJECTS {
            return Err(InvalidVoiceObject);
        }
        let header = header_size(rest)?;
        if rest.len() < header {
            return Err(InvalidVoiceObject);
        }
        let length_at = header.checked_sub(4).ok_or(InvalidVoiceObject)?;
        let length = usize::try_from(u32::from_le_bytes(field(rest, length_at)?))
            .map_err(|_| InvalidVoiceObject)?;
        let end = header.checked_add(length).ok_or(InvalidVoiceObject)?;
        let (object, tail) = rest.split_at_checked(end).ok_or(InvalidVoiceObject)?;
        validate(object)?;
        objects.push(object);
        rest = tail;
    }
    Ok(objects)
}

/// Relay frame/datagram validator: a single object or a redundancy bundle.
///
/// # Errors
///
/// Returns [`InvalidVoiceObject`] whenever [`bundle_objects`] would.
pub fn validate_bundle(bytes: &[u8]) -> Result<(), InvalidVoiceObject> {
    bundle_objects(bytes).map(|_| ())
}

/// The sequence and 48 kHz timestamp of one object [`validate`] accepted.
fn position(object: &[u8]) -> Result<(u64, u64), InvalidVoiceObject> {
    Ok((
        u64::from_le_bytes(field(object, 4)?),
        u64::from_le_bytes(field(object, 12)?),
    ))
}

/// Loss at or above this (per mille) adds one earlier object to each bundle:
/// an isolated loss is repaired 20 ms later.
pub const REDUNDANCY_ONE_PERMILLE: u64 = 10;
/// Loss at or above this (per mille) adds two: a 40 ms burst is repaired.
pub const REDUNDANCY_TWO_PERMILLE: u64 = 50;
/// Loss is measured over windows at least this long...
pub const LOSS_WINDOW: std::time::Duration = std::time::Duration::from_secs(1);
/// ...that sent at least this many packets: a silent (DTX) hop sends ~3 per
/// second and keeps accumulating until a window is meaningful.
pub const LOSS_MIN_PACKETS: u64 = 20;
/// Redundancy steps down one level only after loss stayed below the current
/// level's threshold this long; it steps up within one window.
pub const REDUNDANCY_HOLD: std::time::Duration = std::time::Duration::from_secs(5);

/// How many earlier objects one hop adds to each datagram bundle, from the
/// loss that hop's transport measured. Redundancy costs a full
/// object per copy, so it is spent only where loss is observed: zero on a
/// clean hop, one at [`REDUNDANCY_ONE_PERMILLE`], two at
/// [`REDUNDANCY_TWO_PERMILLE`].
#[derive(Clone, Copy, Debug, Default)]
pub struct Redundancy {
    level: u8,
    window: Option<(std::time::Instant, u64, u64)>,
    calm_since: Option<std::time::Instant>,
}

impl Redundancy {
    /// Earlier objects to add to each bundle now.
    #[must_use]
    pub fn level(&self) -> usize {
        usize::from(self.level)
    }

    /// Feed the hop's cumulative sent and lost packet counts; returns the
    /// level to use from now on.
    pub fn observe(&mut self, now: std::time::Instant, sent: u64, lost: u64) -> usize {
        let Some((start, sent_before, lost_before)) = self.window else {
            self.window = Some((now, sent, lost));
            return self.level();
        };
        let packets = sent.saturating_sub(sent_before);
        if now.saturating_duration_since(start) < LOSS_WINDOW || packets < LOSS_MIN_PACKETS {
            return self.level();
        }
        self.window = Some((now, sent, lost));
        let permille = lost
            .saturating_sub(lost_before)
            .saturating_mul(1000)
            .checked_div(packets)
            .unwrap_or(0);
        let target = [REDUNDANCY_ONE_PERMILLE, REDUNDANCY_TWO_PERMILLE]
            .iter()
            .map(|threshold| u8::from(permille >= *threshold))
            .fold(0, u8::saturating_add);
        if target >= self.level {
            self.level = target;
            self.calm_since = None;
        } else {
            let since = *self.calm_since.get_or_insert(start);
            if now.saturating_duration_since(since) >= REDUNDANCY_HOLD {
                self.level = self.level.saturating_sub(1);
                self.calm_since = (self.level > target).then_some(now);
            }
        }
        self.level()
    }
}

/// One hop's re-bundling of a voice datagram stream: forwards each object
/// once, plus up to the hop's [`Redundancy`] level of the objects forwarded
/// just before it. A bundle that carries nothing new (a redundant copy whose
/// original already arrived) is not forwarded at all, so a publisher's own
/// redundancy (older browsers always send three objects) never costs a clean
/// downlink anything.
#[derive(Debug, Default)]
pub struct Rebundler {
    /// The most recently forwarded objects, oldest first.
    recent: std::collections::VecDeque<(u64, u64, Vec<u8>)>,
    newest: Option<u64>,
}

/// A sequence this far behind the newest forwarded object is a restarted
/// publisher, not a late datagram.
const RESTART_DISTANCE: u64 = 256;

impl Rebundler {
    /// The bundle to forward for one received `bundle` with `redundancy`
    /// earlier objects, or `None` when it carries no object not yet forwarded.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidVoiceObject`] whenever [`bundle_objects`] would.
    pub fn next(
        &mut self,
        bundle: &[u8],
        redundancy: usize,
    ) -> Result<Option<Vec<u8>>, InvalidVoiceObject> {
        let mut fresh = Vec::with_capacity(MAX_BUNDLE_OBJECTS);
        for object in bundle_objects(bundle)? {
            let (sequence, timestamp) = position(object)?;
            fresh.push((sequence, timestamp, object));
        }
        let highest = fresh.iter().map(|(sequence, ..)| *sequence).max();
        if let (Some(newest), Some(highest)) = (self.newest, highest)
            && highest.saturating_add(RESTART_DISTANCE) < newest
        {
            self.recent.clear();
            self.newest = None;
        }
        fresh.retain(|(sequence, ..)| self.newest.is_none_or(|newest| *sequence > newest));
        // Newest first, as every bundle is ordered.
        fresh.sort_unstable_by_key(|(sequence, ..)| std::cmp::Reverse(*sequence));
        let Some(&(newest, newest_timestamp, _)) = fresh.first() else {
            return Ok(None);
        };
        let oldest = fresh.last().map_or(newest, |(sequence, ..)| *sequence);
        let mut out: Vec<u8> = fresh
            .iter()
            .flat_map(|(.., object)| object.iter().copied())
            .collect();
        let mut count = fresh.len();
        // Only objects from the frames just before: a redundant copy of a
        // frame from before a DTX pause would arrive long after its playout.
        let horizon = u64::try_from(MAX_BUNDLE_OBJECTS)
            .unwrap_or(u64::MAX)
            .saturating_mul(u64::from(FRAME_SAMPLES));
        for (sequence, timestamp, object) in self.recent.iter().rev() {
            if count >= MAX_BUNDLE_OBJECTS.min(fresh.len().saturating_add(redundancy)) {
                break;
            }
            if *sequence >= oldest
                || newest_timestamp.saturating_sub(*timestamp) > horizon
                || out.len().saturating_add(object.len()) > MAX_BUNDLE_BYTES
            {
                continue;
            }
            out.extend_from_slice(object);
            count = count.saturating_add(1);
        }
        for (sequence, timestamp, object) in fresh.into_iter().rev() {
            self.recent
                .push_back((sequence, timestamp, object.to_vec()));
        }
        while self.recent.len() >= MAX_BUNDLE_OBJECTS {
            self.recent.pop_front();
        }
        self.newest = Some(newest);
        Ok(Some(out))
    }
}
