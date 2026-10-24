//! The Tilo call's speakers as one Opus stream for Discord, a frame every
//! 20 ms.
//!
//! Each speaker's frames wait in a short jitter buffer: a speaker starts
//! playing once two frames are queued and keeps at most three, so a late
//! frame is waited for briefly and a backlog never grows latency. The
//! common case stays cheap: with exactly one speaker playing their Opus
//! frame goes out as it came, never decoded. Only with two or more are the
//! frames decoded, summed with saturation and encoded once.

use std::collections::{BTreeMap, VecDeque};

use opus::{Application, Bitrate, Channels, Decoder, Encoder};

/// Frames a speaker needs queued before they start playing.
const PREBUFFER: usize = 2;
/// Frames a speaker's buffer holds; older ones are dropped.
const DEPTH: usize = 3;
/// Ticks without a frame after which a speaker's state is dropped.
const FORGET_AFTER: u32 = 50;
const SAMPLES: usize = 960;
const STEREO: usize = 2;
const BITRATE: i32 = 64_000;
/// Opus caps a packet at 1275 bytes per frame.
const MAX_PACKET: usize = 1275;

struct Speaker {
    queue: VecDeque<Vec<u8>>,
    playing: bool,
    idle_ticks: u32,
    decoder: Option<Decoder>,
}

#[derive(Default)]
pub struct Mixer {
    speakers: BTreeMap<String, Speaker>,
    encoder: Option<Encoder>,
}

impl Mixer {
    /// A frame `speaker` said.
    pub fn push(&mut self, speaker: &str, opus: Vec<u8>) {
        let entry = self
            .speakers
            .entry(speaker.to_owned())
            .or_insert_with(|| Speaker {
                queue: VecDeque::with_capacity(DEPTH),
                playing: false,
                idle_ticks: 0,
                decoder: None,
            });
        if entry.queue.len() >= DEPTH {
            entry.queue.pop_front();
        }
        entry.queue.push_back(opus);
    }

    /// The frame for the next 20 ms, `None` when nobody is speaking.
    pub fn tick(&mut self) -> Option<Vec<u8>> {
        let mut frames: Vec<(String, Vec<u8>)> = Vec::new();
        for (name, speaker) in &mut self.speakers {
            if !speaker.playing && speaker.queue.len() >= PREBUFFER {
                speaker.playing = true;
            }
            let frame = if speaker.playing {
                speaker.queue.pop_front()
            } else {
                None
            };
            if let Some(frame) = frame {
                speaker.idle_ticks = 0;
                frames.push((name.clone(), frame));
            } else {
                // Ran dry: wait for the buffer to fill again.
                speaker.playing = false;
                speaker.idle_ticks = speaker.idle_ticks.saturating_add(1);
            }
        }
        self.speakers
            .retain(|_, speaker| speaker.idle_ticks < FORGET_AFTER);
        match frames.len() {
            0 => None,
            1 => frames.pop().map(|(_, frame)| frame),
            _ => self.mix(&frames),
        }
    }

    fn mix(&mut self, frames: &[(String, Vec<u8>)]) -> Option<Vec<u8>> {
        let mut sum = vec![0_i16; SAMPLES.saturating_mul(STEREO)];
        let mut pcm = vec![0_i16; SAMPLES.saturating_mul(STEREO)];
        for (name, frame) in frames {
            let Some(speaker) = self.speakers.get_mut(name) else {
                continue;
            };
            if speaker.decoder.is_none() {
                speaker.decoder = Decoder::new(48_000, Channels::Stereo).ok();
            }
            let Some(decoder) = speaker.decoder.as_mut() else {
                continue;
            };
            let Ok(samples) = decoder.decode(frame, &mut pcm, false) else {
                continue;
            };
            let used = samples.saturating_mul(STEREO).min(sum.len());
            for (total, sample) in sum.iter_mut().zip(pcm.iter()).take(used) {
                *total = total.saturating_add(*sample);
            }
        }
        if self.encoder.is_none() {
            self.encoder = Encoder::new(48_000, Channels::Stereo, Application::Audio)
                .ok()
                .and_then(|mut encoder| {
                    encoder.set_bitrate(Bitrate::Bits(BITRATE)).ok()?;
                    Some(encoder)
                });
        }
        self.encoder.as_mut()?.encode_vec(&sum, MAX_PACKET).ok()
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::cast_possible_truncation,
        clippy::cast_precision_loss,
        clippy::arithmetic_side_effects,
        clippy::indexing_slicing,
        reason = "in a test these are the assertion, not a hazard"
    )]

    use super::*;

    fn tone(hz: f32, frame: usize) -> Vec<u8> {
        let mut encoder = Encoder::new(48_000, Channels::Mono, Application::Audio).unwrap();
        let pcm: Vec<i16> = (0..SAMPLES)
            .map(|i| {
                let t = (frame * SAMPLES + i) as f32 / 48_000.0;
                (f32::sin(t * hz * std::f32::consts::TAU) * 8000.0) as i16
            })
            .collect();
        encoder.encode_vec(&pcm, MAX_PACKET).unwrap()
    }

    #[test]
    fn one_speaker_passes_through_after_the_prebuffer() {
        let mut mixer = Mixer::default();
        let (a, b) = (tone(440.0, 0), tone(440.0, 1));
        mixer.push("ada", a.clone());
        assert_eq!(mixer.tick(), None, "waiting for a second frame");
        mixer.push("ada", b.clone());
        assert_eq!(mixer.tick(), Some(a), "the same bytes, never decoded");
        assert_eq!(mixer.tick(), Some(b));
        assert_eq!(mixer.tick(), None, "ran dry");
    }

    #[test]
    fn a_backlog_is_cut_to_the_buffer_depth() {
        let mut mixer = Mixer::default();
        let frames: Vec<Vec<u8>> = (0..6).map(|i| tone(440.0, i)).collect();
        for frame in &frames {
            mixer.push("ada", frame.clone());
        }
        assert_eq!(
            mixer.tick().as_ref(),
            Some(&frames[3]),
            "the oldest dropped"
        );
    }

    #[test]
    fn two_speakers_are_mixed_into_one_frame() {
        let mut mixer = Mixer::default();
        for i in 0..2 {
            mixer.push("ada", tone(440.0, i));
            mixer.push("bob", tone(660.0, i));
        }
        let frame = mixer.tick().unwrap();
        assert_ne!(frame, tone(440.0, 0));
        assert_ne!(frame, tone(660.0, 0));
        let mut decoder = Decoder::new(48_000, Channels::Stereo).unwrap();
        let mut pcm = vec![0_i16; SAMPLES * STEREO];
        assert_eq!(decoder.decode(&frame, &mut pcm, false).unwrap(), SAMPLES);
    }
}
