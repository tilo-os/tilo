//! An app's sound on the device's output, over the connection its launcher handed it.
//!
//! A sandboxed app receives neither an audio socket nor the device's mixer: it decodes its own
//! sound and hands the device 20 ms of stereo 48 kHz PCM at a time. On an app's line each
//! [`Request`] is one command, and a write names only how many bytes of PCM follow it, so
//! samples travel as binary and never as JSON. The device answers with the [`Snapshot`] of the
//! output it acted on. Both ends hold to the bounds here: one write carries at most
//! [`MAX_SAMPLES`], an output queues at most [`MAX_QUEUED_FRAMES`], and a command gives up after
//! [`DEADLINE`].

use std::time::Duration;

use serde::{Deserialize, Serialize};

pub const CHANNELS: u16 = 2;
/// The most samples one write may carry: 20 ms of stereo 48 kHz.
pub const MAX_SAMPLES: usize = 1920;
/// The most bytes one write's binary body may carry.
pub const MAX_WRITE_BYTES: usize = MAX_SAMPLES * size_of::<i16>();
/// The most frames one output holds while the device catches up.
pub const MAX_QUEUED_FRAMES: u32 = 7680;
/// How long one command waits for the device before the app gives up on it.
pub const DEADLINE: Duration = Duration::from_secs(3);

/// One command of an app's sound output. A write says how many bytes of PCM follow it on the
/// connection, never the samples themselves.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Open {},
    Write {
        generation: u64,
        sequence: u64,
        bytes: u32,
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

/// What the device's output looks like after a command.
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
    #[must_use]
    pub fn valid(&self) -> bool {
        self.generation > 0
            && self.queued_frames <= MAX_QUEUED_FRAMES
            && !(self.closed && self.ready)
    }
}

impl Request {
    /// Whether this request is inside the bounds both ends hold to. An out-of-bounds write has
    /// no body to read.
    #[must_use]
    pub fn valid(&self) -> bool {
        match self {
            Self::Open {} => true,
            Self::Write {
                generation,
                sequence,
                bytes,
            } => {
                *generation > 0
                    && *sequence > 0
                    && usize::try_from(*bytes).is_ok_and(|bytes| {
                        bytes > 0
                            && bytes <= MAX_WRITE_BYTES
                            && bytes.is_multiple_of(usize::from(CHANNELS))
                    })
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

    /// How many bytes of PCM follow this request on the connection.
    #[must_use]
    pub fn body_len(&self) -> usize {
        match self {
            Self::Write { bytes, .. } if self.valid() => usize::try_from(*bytes).unwrap_or(0),
            _ => 0,
        }
    }

    /// This write's PCM, as the output queue takes it.
    ///
    /// # Errors
    /// The body is not whole stereo samples, or not the length the request named.
    pub fn samples_of(&self, body: &[u8]) -> Result<Vec<i16>, &'static str> {
        match self {
            Self::Write { bytes, .. } => {
                let bytes = usize::try_from(*bytes).map_err(|_| "invalid audio output write")?;
                if body.len() != bytes || !bytes.is_multiple_of(usize::from(CHANNELS)) {
                    return Err("invalid audio output write body");
                }
                let mut samples = Vec::with_capacity(bytes / 2);
                let mut rest = body;
                while let [low, high, tail @ ..] = rest {
                    samples.push(i16::from_le_bytes([*low, *high]));
                    rest = tail;
                }
                Ok(samples)
            }
            _ => Ok(Vec::new()),
        }
    }
}

/// The binary body one write's samples travel as, little-endian.
#[must_use]
pub fn body_of(samples: &[i16]) -> Vec<u8> {
    samples
        .iter()
        .flat_map(|sample| sample.to_le_bytes())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_output_request_names_no_capture_identity_path_or_unbounded_pcm() {
        for bytes in [
            br#"{"op":"capture"}"#.as_slice(),
            br#"{"op":"open","app_id":"so.tilo.settings"}"#,
            br#"{"op":"open","path":"/run/user/1000/pipewire-0"}"#,
        ] {
            assert!(serde_json::from_slice::<Request>(bytes).is_err());
        }
        let write = Request::Write {
            generation: 1,
            sequence: 1,
            bytes: 3840,
        };
        assert!(write.valid());
        assert_eq!(write.body_len(), 3840);
        assert!(
            !Request::Write {
                generation: 1,
                sequence: 0,
                bytes: 2,
            }
            .valid()
        );
        let samples: Vec<i16> = (0..MAX_SAMPLES)
            .map(|at| i16::try_from(at).expect("a bounded sample") - 500)
            .collect();
        assert_eq!(
            write
                .samples_of(&body_of(&samples))
                .expect("a bounded write"),
            samples
        );
        assert!(
            Request::Write {
                generation: 1,
                sequence: 1,
                bytes: 3,
            }
            .samples_of(&[1, 2, 3])
            .is_err(),
            "half a sample is no packet"
        );
    }
}
