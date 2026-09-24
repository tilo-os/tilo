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

impl VoiceObject {
    pub fn encode(&self) -> Result<Vec<u8>, InvalidVoiceObject> {
        if self.duration != FRAME_SAMPLES
            || self.payload.is_empty()
            || self.payload.len() > MAX_OBJECT_BYTES - HEADER_BYTES
        {
            return Err(InvalidVoiceObject);
        }
        let mut bytes = vec![0; HEADER_BYTES + self.payload.len()];
        bytes[0] = 1;
        bytes[4..12].copy_from_slice(&self.sequence.to_le_bytes());
        bytes[12..20].copy_from_slice(&self.timestamp.to_le_bytes());
        bytes[20..22].copy_from_slice(&self.duration.to_le_bytes());
        bytes[22..26].copy_from_slice(&SAMPLE_RATE.to_le_bytes());
        bytes[26] = CHANNELS;
        bytes[28..32].copy_from_slice(&(self.payload.len() as u32).to_le_bytes());
        bytes[HEADER_BYTES..].copy_from_slice(&self.payload);
        Ok(bytes)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, InvalidVoiceObject> {
        validate(bytes)?;
        Ok(Self {
            sequence: u64::from_le_bytes(bytes[4..12].try_into().map_err(|_| InvalidVoiceObject)?),
            timestamp: u64::from_le_bytes(
                bytes[12..20].try_into().map_err(|_| InvalidVoiceObject)?,
            ),
            duration: FRAME_SAMPLES,
            payload: bytes[HEADER_BYTES..].to_vec(),
        })
    }
}

/// Validate without allocating or copying the encoded payload. Used at the
/// relay's complete-frame boundary before subscribers can observe an object.
pub fn validate(bytes: &[u8]) -> Result<(), InvalidVoiceObject> {
    if !(HEADER_BYTES + 1..=MAX_OBJECT_BYTES).contains(&bytes.len()) {
        return Err(InvalidVoiceObject);
    }
    if bytes[..4] != [1, 0, 0, 0]
        || bytes[20..22] != FRAME_SAMPLES.to_le_bytes()
        || bytes[22..26] != SAMPLE_RATE.to_le_bytes()
        || bytes[26] != CHANNELS
        || bytes[27] != 0
        || bytes[28..32] != ((bytes.len() - HEADER_BYTES) as u32).to_le_bytes()
    {
        return Err(InvalidVoiceObject);
    }
    Ok(())
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
        if objects.len() == MAX_BUNDLE_OBJECTS || rest.len() < HEADER_BYTES {
            return Err(InvalidVoiceObject);
        }
        let length =
            u32::from_le_bytes(rest[28..32].try_into().map_err(|_| InvalidVoiceObject)?) as usize;
        let end = HEADER_BYTES
            .checked_add(length)
            .filter(|end| *end <= rest.len())
            .ok_or(InvalidVoiceObject)?;
        validate(&rest[..end])?;
        objects.push(&rest[..end]);
        rest = &rest[end..];
    }
    Ok(objects)
}

/// Relay frame/datagram validator: a single object or a redundancy bundle.
pub fn validate_bundle(bytes: &[u8]) -> Result<(), InvalidVoiceObject> {
    bundle_objects(bytes).map(|_| ())
}
