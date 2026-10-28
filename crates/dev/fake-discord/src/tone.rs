//! Tones as Opus frames, and telling which tones a stream of Opus frames
//! holds: how the voice tests say something and check what was heard.

use opus::{Application, Channels, Decoder, Encoder};

/// Samples in one 20 ms frame at 48 kHz.
pub const FRAME_SAMPLES: usize = 960;
const RATE: f32 = 48_000.0;
const AMPLITUDE: f32 = 0.3;
const MAX_PACKET: usize = 1275;

/// `frames` 20 ms Opus frames (48 kHz mono) of a sine at `hz`.
#[must_use]
pub fn sine(hz: f32, frames: usize) -> Vec<Vec<u8>> {
    let Ok(mut encoder) = Encoder::new(48_000, Channels::Mono, Application::Audio) else {
        return Vec::new();
    };
    let mut phase = 0.0_f32;
    let step = hz / RATE * std::f32::consts::TAU;
    let mut pcm = vec![0.0_f32; FRAME_SAMPLES];
    let mut out = Vec::with_capacity(frames);
    for _ in 0..frames {
        for sample in &mut pcm {
            *sample = phase.sin() * AMPLITUDE;
            phase = (phase + step) % std::f32::consts::TAU;
        }
        if let Ok(frame) = encoder.encode_vec_float(&pcm, MAX_PACKET) {
            out.push(frame);
        }
    }
    out
}

/// The frames decoded in order, as mono samples.
#[must_use]
pub fn decode(frames: &[Vec<u8>]) -> Vec<f32> {
    let Ok(mut decoder) = Decoder::new(48_000, Channels::Stereo) else {
        return Vec::new();
    };
    let mut stereo = vec![0.0_f32; FRAME_SAMPLES.saturating_mul(2)];
    let mut mono = Vec::with_capacity(frames.len().saturating_mul(FRAME_SAMPLES));
    for frame in frames {
        let Ok(samples) = decoder.decode_float(frame, &mut stereo, false) else {
            continue;
        };
        mono.extend(
            stereo
                .chunks_exact(2)
                .take(samples)
                .map(|pair| pair.iter().sum::<f32>() / 2.0),
        );
    }
    mono
}

/// How much of the signal's energy is a tone at `hz`: about 1 for that
/// tone alone, about half for each of two equally loud tones, near 0 for
/// a tone that is not there (Goertzel's single-bin DFT, against the total
/// energy).
#[must_use]
pub fn share(samples: &[f32], hz: f32) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let coefficient = 2.0 * (std::f32::consts::TAU * hz / RATE).cos();
    let (mut previous, mut before) = (0.0_f32, 0.0_f32);
    for &sample in samples {
        let current = sample + coefficient * previous - before;
        before = previous;
        previous = current;
    }
    let power = previous * previous + before * before - coefficient * previous * before;
    let energy: f32 = samples.iter().map(|s| s * s).sum();
    if energy <= f32::EPSILON {
        return 0.0;
    }
    #[allow(
        clippy::cast_precision_loss,
        reason = "a sample count, far below 2^24 in practice"
    )]
    let half = samples.len() as f32 / 2.0;
    power / (half * energy)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "in a test this is the assertion")]

    use super::*;

    #[test]
    fn a_tone_survives_opus_and_is_told_from_another() {
        let samples = decode(&sine(440.0, 50));
        let steady = &samples[FRAME_SAMPLES * 5..];
        assert!(share(steady, 440.0) > 0.7, "{}", share(steady, 440.0));
        assert!(share(steady, 660.0) < 0.05, "{}", share(steady, 660.0));
    }

    #[test]
    fn two_tones_each_hold_part_of_the_energy() {
        let (a, b) = (decode(&sine(440.0, 50)), decode(&sine(660.0, 50)));
        let mixed: Vec<f32> = a.iter().zip(&b).map(|(x, y)| x + y).collect();
        let steady = &mixed[FRAME_SAMPLES * 5..];
        assert!(share(steady, 440.0) > 0.3);
        assert!(share(steady, 660.0) > 0.3);
    }
}
