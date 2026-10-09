//! Bounded native decoding with immutable confirmed audio prefixes.
use super::{MAX_SAMPLES, MAX_TEXT_BYTES, SAMPLE_RATE, SpeechEngine, SpeechError};
use std::ffi::CString;
use zeroize::{Zeroize as _, Zeroizing};

const TARGET: usize = 20 * SAMPLE_RATE;
const INITIAL_DRAFT_SAMPLES: usize = 2 * SAMPLE_RATE;
const PADDING: usize = TARGET / 4;
const QUIET_WINDOW: usize = SAMPLE_RATE / 10;
const LOOKAHEAD: usize = TARGET + PADDING + QUIET_WINDOW;

/// One worker session. Audio must be the append-only buffer of its `SpeechInput`.
/// Confirmed prefixes are decoded once; only the unfinished tail is revised.
/// Cancellation still terminates the owned worker, including native inference.
#[derive(Default)]
pub struct SpeechRecognition {
    consumed: usize,
    observed: usize,
    confirmed: Zeroizing<String>,
    draft: Zeroizing<String>,
    draft_samples: Option<usize>,
    retired: bool,
}

impl SpeechRecognition {
    /// Decode bounded quiet-boundary segments through the production engine.
    /// Complete consumes the session; failures cannot be retried against a cache.
    /// # Errors
    /// Rejects invalid/non-append audio, native failure and oversized text.
    pub fn transcribe(
        &mut self,
        engine: &mut SpeechEngine,
        samples: &[f32],
        language: &CString,
        complete: bool,
    ) -> Result<String, SpeechError> {
        self.recognize_with(samples, complete, |chunk| {
            engine.transcribe(chunk, language)
        })
    }

    /// Use the same bounded segmentation with another image-owned decoder.
    /// # Errors
    /// Rejects invalid audio, retired sessions and decoder errors.
    pub fn recognize_with(
        &mut self,
        samples: &[f32],
        complete: bool,
        recognize: impl FnMut(&[f32]) -> Result<String, SpeechError>,
    ) -> Result<String, SpeechError> {
        let deferred = !complete && samples.len() < INITIAL_DRAFT_SAMPLES;
        let result = if self.retired || samples.len() < self.observed {
            Err(SpeechError::Protocol)
        } else if samples.len() > MAX_SAMPLES || samples.iter().any(|s| !s.is_finite()) {
            Err(SpeechError::AudioTooLong)
        } else if samples.iter().all(|sample| *sample == 0.0) {
            // Exact silence needs no inference, including image-owned engines.
            // Do not use an energy threshold: quiet speech must still decode.
            self.observed = samples.len();
            Ok(String::new())
        } else if deferred {
            // A truncated opening word produced confident hallucinations in
            // paced public speech. Wait for initial context, without decoding
            // or caching an empty hypothesis as a reusable final result.
            self.observed = samples.len();
            Ok(String::new())
        } else if self.draft_samples == Some(samples.len())
            && (!complete || samples.len().saturating_sub(self.consumed) <= TARGET + PADDING)
        {
            // Finish often has exactly the last draft's audio. Native decoding
            // is deterministic; no further cut is needed for this short tail.
            Ok(self.draft.as_str().to_owned())
        } else {
            self.decode(samples, complete, recognize)
        };
        self.retired |= complete || result.is_err();
        if self.retired {
            self.confirmed.zeroize();
            self.draft.zeroize();
            self.draft_samples = None;
        } else if !deferred && let Ok(text) = &result {
            self.draft.zeroize();
            self.draft.push_str(text);
            self.draft_samples = Some(samples.len());
        }
        result
    }

    fn decode(
        &mut self,
        samples: &[f32],
        complete: bool,
        mut recognize: impl FnMut(&[f32]) -> Result<String, SpeechError>,
    ) -> Result<String, SpeechError> {
        self.observed = samples.len();
        let mut remaining = samples.get(self.consumed..).ok_or(SpeechError::Protocol)?;
        // Drafts wait for the entire energy scan, so subsequent appended audio
        // cannot change a confirmed cut. Final uses the measured batch rule.
        while remaining.len() > LOOKAHEAD || (complete && remaining.len() > TARGET + PADDING) {
            let cut = quiet_boundary(remaining);
            let (chunk, tail) = remaining.split_at(cut);
            let part = Zeroizing::new(recognize(chunk)?);
            append(&mut self.confirmed, &part)?;
            self.consumed = self
                .consumed
                .checked_add(cut)
                .ok_or(SpeechError::Protocol)?;
            remaining = tail;
        }
        let mut result = self.confirmed.clone();
        if !remaining.is_empty() {
            let part = Zeroizing::new(recognize(remaining)?);
            append(&mut result, &part)?;
        }
        Ok(result.as_str().to_owned())
    }
}

fn quiet_boundary(samples: &[f32]) -> usize {
    (TARGET - PADDING..TARGET + PADDING)
        .step_by(SAMPLE_RATE / 50)
        .filter_map(|offset| {
            let window = samples.get(offset..offset.checked_add(QUIET_WINDOW)?)?;
            let energy = window.iter().map(|s| f64::from(*s).powi(2)).sum::<f64>();
            Some((offset.checked_add(QUIET_WINDOW / 2)?, energy))
        })
        .min_by(|left, right| left.1.total_cmp(&right.1))
        .map_or(TARGET, |(cut, _)| cut)
}

fn append(output: &mut String, part: &str) -> Result<(), SpeechError> {
    let part = part.trim();
    let separator = usize::from(!output.is_empty() && !part.is_empty());
    let extra = part
        .len()
        .checked_add(separator)
        .ok_or(SpeechError::TextTooLong)?;
    if output.len().saturating_add(extra) > MAX_TEXT_BYTES {
        return Err(SpeechError::TextTooLong);
    }
    output
        .try_reserve_exact(extra)
        .map_err(|_| SpeechError::TextTooLong)?;
    if separator != 0 {
        output.push(' ');
    }
    output.push_str(part);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_silence_never_decodes_but_appended_quiet_audio_does() {
        let mut audio = vec![0.0; INITIAL_DRAFT_SAMPLES];
        audio[0] = -0.0;
        let mut recognition = SpeechRecognition::default();
        assert_eq!(
            recognition.recognize_with(&audio, false, |_| panic!("silence must not infer")),
            Ok(String::new())
        );
        audio.push(f32::MIN_POSITIVE);
        let mut calls = 0;
        assert_eq!(
            recognition.recognize_with(&audio, true, |chunk| {
                assert_eq!(chunk, audio);
                calls += 1;
                Ok("quiet speech".into())
            }),
            Ok("quiet speech".into())
        );
        assert_eq!(calls, 1);
        assert!(recognition.retired);
        let mut silent = SpeechRecognition::default();
        assert_eq!(
            silent.recognize_with(&audio[..INITIAL_DRAFT_SAMPLES], true, |_| panic!(
                "silent final must not infer"
            )),
            Ok(String::new())
        );
        assert!(silent.retired && silent.draft.is_empty());
    }

    #[test]
    fn worker_boundary_keeps_confirmed_prefix_and_revises_only_the_tail() {
        let mut audio = vec![1.0; 50 * SAMPLE_RATE];
        audio[18 * SAMPLE_RATE..19 * SAMPLE_RATE].fill(0.0);
        let mut recognition = SpeechRecognition::default();
        let mut lengths = Vec::new();
        let mut decode = |chunk: &[f32]| {
            lengths.push(chunk.len());
            Ok(if chunk.len() > 18 * SAMPLE_RATE {
                "confirmed"
            } else {
                "draft"
            }
            .into())
        };
        let first = recognition
            .decode(&audio[..26 * SAMPLE_RATE], false, &mut decode)
            .unwrap();
        assert_eq!(first, "confirmed draft");
        let consumed = recognition.consumed;
        assert!((18 * SAMPLE_RATE..19 * SAMPLE_RATE).contains(&consumed));
        lengths.clear();
        let next = recognition
            .decode(&audio[..30 * SAMPLE_RATE], false, |chunk| {
                lengths.push(chunk.len());
                Ok("revised".into())
            })
            .unwrap();
        assert_eq!(next, "confirmed revised");
        assert_eq!(lengths, [30 * SAMPLE_RATE - consumed]);
        lengths.clear();
        let final_text = recognition
            .decode(&audio, true, |chunk| {
                lengths.push(chunk.len());
                Ok("final".into())
            })
            .unwrap();
        assert_eq!(final_text, "confirmed final final");
        assert!(
            lengths
                .iter()
                .all(|length| *length <= TARGET + PADDING + QUIET_WINDOW)
        );
        assert_eq!(lengths.iter().sum::<usize>() + consumed, audio.len());
    }

    #[test]
    fn deferred_opening_draft_never_erases_a_short_final_or_accepts_audio_regression() {
        let audio = vec![0.5; SAMPLE_RATE];
        let mut recognition = SpeechRecognition::default();
        assert_eq!(
            recognition.recognize_with(&audio, false, |_| panic!("opening context must wait")),
            Ok(String::new())
        );
        assert_eq!(recognition.observed, audio.len());
        assert!(
            recognition.draft_samples.is_none(),
            "deferred output is not a decoded cache"
        );
        let mut calls = 0;
        assert_eq!(
            recognition.recognize_with(&audio, true, |chunk| {
                assert_eq!(chunk, audio);
                calls += 1;
                Ok("short final 👋".into())
            }),
            Ok("short final 👋".into())
        );
        assert_eq!(
            calls, 1,
            "stopping before two seconds still decodes all recorded audio"
        );
        assert!(recognition.retired);
        assert!(recognition.draft.is_empty());
        let mut recognition = SpeechRecognition::default();
        recognition
            .recognize_with(&audio, false, |_| panic!("opening context must wait"))
            .unwrap();
        assert_eq!(
            recognition.recognize_with(&audio[..SAMPLE_RATE / 2], false, |_| panic!(
                "regressed audio cannot decode"
            )),
            Err(SpeechError::Protocol)
        );
        assert!(recognition.retired);
    }

    #[test]
    fn finish_reuses_unchanged_audio_but_never_skips_a_pending_cut() {
        let audio = vec![0.5; INITIAL_DRAFT_SAMPLES];
        let mut recognition = SpeechRecognition::default();
        assert_eq!(
            recognition
                .recognize_with(&audio, false, |_| Ok("draft".into()))
                .unwrap(),
            "draft"
        );
        assert_eq!(
            recognition
                .recognize_with(&audio, true, |_| panic!("unchanged final must reuse draft"))
                .unwrap(),
            "draft"
        );
        assert!(recognition.retired);
        assert!(recognition.draft.is_empty());
        assert_eq!(
            recognition.recognize_with(&audio, true, |_| panic!("retired worker cannot decode")),
            Err(SpeechError::Protocol)
        );
        let audio = vec![0.5; TARGET + PADDING + 1];
        let mut recognition = SpeechRecognition::default();
        recognition
            .recognize_with(&audio, false, |_| Ok("uncut".into()))
            .unwrap();
        let mut calls = 0;
        let text = recognition
            .recognize_with(&audio, true, |_| {
                calls += 1;
                Ok("cut".into())
            })
            .unwrap();
        assert_eq!(calls, 2);
        assert_eq!(text, "cut cut");
    }

    #[test]
    fn worker_boundary_preserves_short_input_and_refuses_combined_overflow() {
        let audio = vec![0.5; 5 * SAMPLE_RATE];
        let mut recognition = SpeechRecognition::default();
        assert_eq!(
            recognition
                .decode(&audio, true, |chunk| {
                    assert_eq!(chunk, audio);
                    Ok("original short output".into())
                })
                .unwrap(),
            "original short output"
        );
        let audio = vec![0.5; 50 * SAMPLE_RATE];
        let mut recognition = SpeechRecognition::default();
        let error = recognition
            .decode(&audio, true, |_| Ok("x".repeat(MAX_TEXT_BYTES)))
            .unwrap_err();
        assert_eq!(error, SpeechError::TextTooLong);
    }
}
