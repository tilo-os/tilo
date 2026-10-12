//! Device-local PCM output. No device paths, capture operations or caller identity cross this wire.
use serde::{Deserialize, Serialize};

pub const MAX_WIRE_BYTES: usize = 32 * 1024;
pub const MAX_SAMPLES: usize = 1920;
pub const MAX_QUEUED_FRAMES: u32 = 7680;
pub const RATE: u32 = 48_000;
pub const CHANNELS: u16 = 2;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    Open {},
    Write {
        generation: u64,
        sequence: u64,
        samples: Vec<i16>,
    },
    Pause {
        generation: u64,
        paused: bool,
    },
    Volume {
        generation: u64,
        percent: u8,
    },
    Flush {
        generation: u64,
    },
    Status {
        generation: u64,
    },
    Close {
        generation: u64,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub generation: u64,
    pub accepted_sequence: u64,
    pub rendered_frames: u64,
    pub queued_frames: u32,
    pub paused: bool,
    pub ready: bool,
    pub closed: bool,
}

impl Snapshot {
    pub fn valid(&self) -> bool {
        self.generation > 0
            && self.queued_frames <= MAX_QUEUED_FRAMES
            && !(self.closed && self.ready)
    }
}

impl Command {
    pub fn valid(&self) -> bool {
        match self {
            Self::Open {} => true,
            Self::Write {
                generation,
                sequence,
                samples,
            } => {
                *generation > 0
                    && *sequence > 0
                    && !samples.is_empty()
                    && samples.len() <= MAX_SAMPLES
                    && samples.len().is_multiple_of(usize::from(CHANNELS))
            }
            Self::Volume {
                generation,
                percent,
            } => *generation > 0 && *percent <= 100,
            Self::Pause { generation, .. }
            | Self::Flush { generation }
            | Self::Status { generation }
            | Self::Close { generation } => *generation > 0,
        }
    }

    /// Encode a bounded, closed output command.
    ///
    /// # Errors
    /// Refuses invalid parameters or oversized serialization.
    pub fn encode(&self) -> Result<Vec<u8>, &'static str> {
        if !self.valid() {
            return Err("invalid audio output command");
        }
        let bytes = serde_json::to_vec(self).map_err(|_| "audio output encoding failed")?;
        if bytes.len() > MAX_WIRE_BYTES {
            return Err("audio output command too large");
        }
        Ok(bytes)
    }

    /// Decode one bounded command; unknown fields and capture operations fail.
    ///
    /// # Errors
    /// Refuses invalid, oversized or unknown commands.
    pub fn decode(bytes: &[u8]) -> Result<Self, &'static str> {
        if bytes.len() > MAX_WIRE_BYTES {
            return Err("audio output command too large");
        }
        let command: Self =
            serde_json::from_slice(bytes).map_err(|_| "invalid audio output encoding")?;
        command
            .valid()
            .then_some(command)
            .ok_or("invalid audio output command")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_wire_rejects_capture_identity_paths_and_unbounded_pcm() {
        for bytes in [
            br#"{"op":"capture"}"#.as_slice(),
            br#"{"op":"open","app_id":"so.tilo.settings"}"#,
            br#"{"op":"open","path":"/run/user/1000/pipewire-0"}"#,
        ] {
            assert!(Command::decode(bytes).is_err());
        }
        let command = Command::Write {
            generation: 1,
            sequence: 1,
            samples: vec![i16::MIN; MAX_SAMPLES],
        };
        let bytes = command.encode().expect("bounded production command");
        assert!(bytes.len() < MAX_WIRE_BYTES);
        assert_eq!(Command::decode(&bytes).expect("production decode"), command);
        assert!(
            Command::Write {
                generation: 1,
                sequence: 1,
                samples: vec![0; MAX_SAMPLES + 2]
            }
            .encode()
            .is_err()
        );
        assert!(
            Command::Write {
                generation: 1,
                sequence: 0,
                samples: vec![0; 2]
            }
            .encode()
            .is_err()
        );
    }
}
