//! A call's speakers as one Opus stream, a frame every 20 ms, for a service
//! whose call takes one stream from the bridge carrying it.
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
    /// A frame of theirs has played: from then on a frame dropped from the
    /// queue leaves a hole in the stream the receiver must hear about.
    started: bool,
    idle_ticks: u32,
    decoder: Option<Decoder>,
}

#[derive(Default)]
pub struct Mixer {
    speakers: BTreeMap<String, Speaker>,
    encoder: Option<Encoder>,
    shed: u32,
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
                started: false,
                idle_ticks: 0,
                decoder: None,
            });
        if entry.queue.len() >= DEPTH {
            let dropped = entry.queue.pop_front();
            if entry.started && dropped.is_some() {
                self.shed = self.shed.saturating_add(1);
            }
        }
        entry.queue.push_back(opus);
    }

    /// How many frames were dropped from streams already playing since this
    /// was last asked: each one is a 20 ms hole the sender must leave in the
    /// RTP timeline for the receiver to conceal.
    pub fn take_shed(&mut self) -> u32 {
        std::mem::take(&mut self.shed)
    }

    /// Whether a speaker is still part of the stream: one has frames queued,
    /// or one played within the ticks after which a silent speaker is
    /// forgotten. A tick with nothing to send while this holds is a late
    /// frame, not the end of what is said.
    #[must_use]
    pub fn active(&self) -> bool {
        !self.speakers.is_empty()
    }

    /// The frame for the next 20 ms, `None` when nobody is speaking.
    pub fn tick(&mut self) -> Option<Vec<u8>> {
        self.tick_said().map(|(_, frame)| frame)
    }

    /// The frame for the next 20 ms and whose it is: the one speaker it came
    /// from untouched, or `None` for a mix of several.
    pub fn tick_said(&mut self) -> Option<(Option<String>, Vec<u8>)> {
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
                speaker.started = true;
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
            1 => frames.pop().map(|(speaker, frame)| (Some(speaker), frame)),
            _ => {
                // The mix is encoded anew from what is queued, so what the
                // receiver gets is continuous: a frame shed from a speaker's
                // queue is no hole in that stream to conceal.
                self.shed = 0;
                self.mix(&frames).map(|frame| (None, frame))
            }
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
        assert_eq!(mixer.tick().as_ref(), Some(&frames[4]));
        assert_eq!(
            mixer.tick().as_ref(),
            Some(&frames[5]),
            "the three newest play"
        );
        assert_eq!(mixer.tick(), None, "and nothing older");
        assert_eq!(
            mixer.take_shed(),
            0,
            "nothing played yet, so no hole in a stream the receiver has"
        );
    }

    #[test]
    fn a_frame_shed_after_playback_started_is_the_senders_to_conceal() {
        let mut mixer = Mixer::default();
        let (a, b) = (tone(440.0, 0), tone(440.0, 1));
        mixer.push("ada", a.clone());
        mixer.push("ada", b);
        assert_eq!(mixer.take_shed(), 0, "nothing lost before playback");
        assert_eq!(mixer.tick(), Some(a));
        let burst: Vec<Vec<u8>> = (2..6).map(|i| tone(440.0, i)).collect();
        for frame in &burst {
            mixer.push("ada", frame.clone());
        }
        assert_eq!(
            mixer.take_shed(),
            2,
            "a burst past the depth sheds its oldest two"
        );
        assert_eq!(mixer.tick().as_ref(), Some(&burst[1]), "the rest play");
    }

    #[test]
    fn a_speaker_stays_active_until_forgotten() {
        let mut mixer = Mixer::default();
        assert!(!mixer.active(), "nobody has said anything");
        mixer.push("ada", tone(440.0, 0));
        mixer.push("ada", tone(440.0, 1));
        assert!(mixer.active(), "a frame is queued");
        assert!(mixer.tick().is_some(), "the first plays");
        assert!(mixer.active());
        assert!(mixer.tick().is_some(), "the second plays");
        assert!(mixer.tick().is_none(), "ran dry");
        assert!(mixer.active(), "a dry tick is a late frame, not the end");
        for _ in 1..FORGET_AFTER {
            assert!(mixer.tick().is_none());
        }
        assert!(
            !mixer.active(),
            "forgotten after {FORGET_AFTER} silent ticks"
        );
    }

    #[test]
    fn a_mixed_tick_has_no_hole_for_the_receiver_to_conceal() {
        let mut mixer = Mixer::default();
        for i in 0..2 {
            mixer.push("ada", tone(440.0, i));
            mixer.push("bob", tone(660.0, i));
        }
        assert!(mixer.tick().is_some(), "the first mix");
        let burst: Vec<Vec<u8>> = (2..6).map(|i| tone(440.0, i)).collect();
        for frame in &burst {
            mixer.push("ada", frame.clone());
        }
        assert!(mixer.tick().is_some(), "still a mix");
        assert_eq!(
            mixer.take_shed(),
            0,
            "the mix is encoded anew, so its stream has no hole"
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

    /// Nobody speaking is no frame, one frame alone never starts, and a
    /// speaker silent for [`FORGET_AFTER`] ticks is forgotten with the frame
    /// they left, not a tick sooner.
    #[test]
    fn a_lone_frame_waits_until_its_speaker_is_forgotten() {
        assert_eq!(Mixer::default().tick(), None, "nobody");
        for (ticks, played) in [(FORGET_AFTER - 1, true), (FORGET_AFTER, false)] {
            let mut mixer = Mixer::default();
            let (first, second) = (tone(440.0, 0), tone(440.0, 1));
            mixer.push("ada", first.clone());
            for tick in 0..ticks {
                assert_eq!(mixer.tick(), None, "tick {tick}: one frame is no start");
            }
            mixer.push("ada", second);
            assert_eq!(
                mixer.tick(),
                played.then_some(first),
                "after {ticks} silent ticks"
            );
        }
    }
}
