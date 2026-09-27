//! The transport-independent tilo-voice/1 object profile.

pub const HEADER_BYTES: usize = 32;
pub const MAX_OBJECT_BYTES: usize = 4096;
pub const FRAME_SAMPLES: u16 = 960;
pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: u8 = 1;
pub const FRAMES_PER_GROUP: u64 = 5;
/// A datagram bundle carries the newest object and up to two earlier ones.
pub const MAX_BUNDLE_OBJECTS: usize = 3;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VoiceObject {
    pub sequence: u64,
    pub timestamp: u64,
    pub duration: u16,
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
    /// Encode this object in the tilo-voice/1 wire layout.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidVoiceObject`] when the duration is not one frame or the
    /// payload is empty or does not fit in [`MAX_OBJECT_BYTES`].
    pub fn encode(&self) -> Result<Vec<u8>, InvalidVoiceObject> {
        if self.duration != FRAME_SAMPLES
            || self.payload.is_empty()
            || self.payload.len() > MAX_OBJECT_BYTES - HEADER_BYTES
        {
            return Err(InvalidVoiceObject);
        }
        let length = u32::try_from(self.payload.len()).map_err(|_| InvalidVoiceObject)?;
        let mut bytes = Vec::with_capacity(HEADER_BYTES.saturating_add(self.payload.len()));
        bytes.extend_from_slice(&[1, 0, 0, 0]);
        bytes.extend_from_slice(&self.sequence.to_le_bytes());
        bytes.extend_from_slice(&self.timestamp.to_le_bytes());
        bytes.extend_from_slice(&self.duration.to_le_bytes());
        bytes.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
        bytes.extend_from_slice(&[CHANNELS, 0]);
        bytes.extend_from_slice(&length.to_le_bytes());
        debug_assert_eq!(bytes.len(), HEADER_BYTES);
        bytes.extend_from_slice(&self.payload);
        Ok(bytes)
    }

    /// Decode one complete, valid tilo-voice/1 object.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidVoiceObject`] when [`validate`] rejects `bytes`.
    pub fn decode(bytes: &[u8]) -> Result<Self, InvalidVoiceObject> {
        validate(bytes)?;
        Ok(Self {
            sequence: u64::from_le_bytes(field(bytes, 4)?),
            timestamp: u64::from_le_bytes(field(bytes, 12)?),
            duration: FRAME_SAMPLES,
            payload: bytes
                .get(HEADER_BYTES..)
                .ok_or(InvalidVoiceObject)?
                .to_vec(),
        })
    }
}

/// Validate without allocating or copying the encoded payload. Used at the
/// relay's complete-frame boundary before subscribers can observe an object.
///
/// # Errors
///
/// Returns [`InvalidVoiceObject`] for a wrong size, version, frame duration,
/// sample rate, channel count or declared payload length.
pub fn validate(bytes: &[u8]) -> Result<(), InvalidVoiceObject> {
    if !(HEADER_BYTES + 1..=MAX_OBJECT_BYTES).contains(&bytes.len()) {
        return Err(InvalidVoiceObject);
    }
    let payload_length =
        u32::try_from(bytes.len().saturating_sub(HEADER_BYTES)).map_err(|_| InvalidVoiceObject)?;
    if field::<4>(bytes, 0)? != [1, 0, 0, 0]
        || field::<2>(bytes, 20)? != FRAME_SAMPLES.to_le_bytes()
        || field::<4>(bytes, 22)? != SAMPLE_RATE.to_le_bytes()
        || field::<2>(bytes, 26)? != [CHANNELS, 0]
        || field::<4>(bytes, 28)? != payload_length.to_le_bytes()
    {
        return Err(InvalidVoiceObject);
    }
    Ok(())
}

/// Split a datagram bundle (one to [`MAX_BUNDLE_OBJECTS`] complete, individually
/// valid objects back to back, newest first) without copying. Stream frames are
/// always single objects, which are one-object bundles.
///
/// # Errors
///
/// Returns [`InvalidVoiceObject`] for an empty or oversized bundle, too many
/// objects, a truncated object, or any object [`validate`] rejects.
pub fn bundle_objects(bytes: &[u8]) -> Result<Vec<&[u8]>, InvalidVoiceObject> {
    if bytes.is_empty() || bytes.len() > MAX_OBJECT_BYTES {
        return Err(InvalidVoiceObject);
    }
    let mut objects = Vec::with_capacity(MAX_BUNDLE_OBJECTS);
    let mut rest = bytes;
    while !rest.is_empty() {
        if objects.len() == MAX_BUNDLE_OBJECTS || rest.len() < HEADER_BYTES {
            return Err(InvalidVoiceObject);
        }
        let length = usize::try_from(u32::from_le_bytes(field(rest, 28)?))
            .map_err(|_| InvalidVoiceObject)?;
        let end = HEADER_BYTES.checked_add(length).ok_or(InvalidVoiceObject)?;
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
