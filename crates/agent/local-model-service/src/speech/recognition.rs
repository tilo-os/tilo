//! Bounded native decoding with immutable confirmed audio prefixes.
use super::{MAX_SAMPLES, MAX_TEXT_BYTES, SAMPLE_RATE, SpeechEngine, SpeechError};
use std::ffi::CString;
use zeroize::{Zeroize as _, Zeroizing};

#[derive(Clone, Copy)]
enum AcousticAdmission {
    Legacy,
    Inactive,
    Speech(usize),
}

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
        self.transcribe_with_activity(engine, None, samples, language, complete)
    }

    /// Decode original PCM after independent acoustic admission. Final speech
    /// never waits for the preview context requirement and never loses samples.
    /// # Errors
    /// Rejects detector failures as well as the ordinary recognition failures.
    pub fn transcribe_with_activity(
        &mut self,
        engine: &mut SpeechEngine,
        activity: Option<&mut super::activity::SpeechActivity>,
        samples: &[f32],
        language: &CString,
        complete: bool,
    ) -> Result<String, SpeechError> {
        let admission = if let Some(graph) = activity {
            graph
                .first_sample(samples)?
                .map_or(AcousticAdmission::Inactive, AcousticAdmission::Speech)
        } else {
            AcousticAdmission::Legacy
        };
        engine.draft_mode(!complete);
        let result = self.recognize_with_admission(
            samples,
            complete,
            language.to_bytes() == b"auto",
            admission,
            |chunk| engine.transcribe(chunk, language),
        );
        engine.draft_mode(false);
        result
    }

    /// Use the same bounded segmentation with another image-owned decoder.
    pub fn recognize_with(
        &mut self,
        samples: &[f32],
        complete: bool,
        recognize: impl FnMut(&[f32]) -> Result<String, SpeechError>,
    ) -> Result<String, SpeechError> {
        self.recognize_with_policy(samples, complete, false, recognize)
    }

    fn recognize_with_policy(
        &mut self,
        samples: &[f32],
        complete: bool,
        automatic: bool,
        recognize: impl FnMut(&[f32]) -> Result<String, SpeechError>,
    ) -> Result<String, SpeechError> {
        self.recognize_with_admission(
            samples,
            complete,
            automatic,
            AcousticAdmission::Legacy,
            recognize,
        )
    }

    fn recognize_with_admission(
        &mut self,
        samples: &[f32],
        complete: bool,
        automatic: bool,
        admission: AcousticAdmission,
        recognize: impl FnMut(&[f32]) -> Result<String, SpeechError>,
    ) -> Result<String, SpeechError> {
        let no_activity = matches!(admission, AcousticAdmission::Inactive);
        let context_start = match admission {
            AcousticAdmission::Speech(first) => first,
            AcousticAdmission::Legacy | AcousticAdmission::Inactive => 0,
        };
        let deferred = no_activity
            || (!complete && samples.len().saturating_sub(context_start) < INITIAL_DRAFT_SAMPLES);

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
            // Independent acoustic inactivity invokes no decoder. Otherwise a
            // truncated opening word produced confident hallucinations in
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
            self.decode_with_policy(samples, complete, automatic, recognize)
        };
        if !complete && matches!(result, Err(SpeechError::DraftInterrupted)) {
            // Each canonical prefix is committed only after native inference
            // completes. Keep those immutable phrases; the interrupted chunk
            // returns no text and never advances consumed. Retire the old
            // draft cache so Finish must decode every remaining PCM sample.
            self.draft.zeroize();
            self.draft_samples = None;
            return result;
        }
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

    #[cfg(test)]
    fn decode(
        &mut self,
        samples: &[f32],
        complete: bool,
        recognize: impl FnMut(&[f32]) -> Result<String, SpeechError>,
    ) -> Result<String, SpeechError> {
        self.decode_with_policy(samples, complete, false, recognize)
    }

    fn decode_with_policy(
        &mut self,
        samples: &[f32],
        complete: bool,
        automatic: bool,
        mut recognize: impl FnMut(&[f32]) -> Result<String, SpeechError>,
    ) -> Result<String, SpeechError> {
        self.observed = samples.len();
        let mut remaining = samples.get(self.consumed..).ok_or(SpeechError::Protocol)?;
        // Drafts wait for the entire energy scan, so subsequent appended audio
        // cannot change a confirmed cut. Final uses the measured batch rule.
        loop {
            let pause = automatic.then(|| pause_boundary(remaining)).flatten();
            let cut = pause.or_else(|| {
                (remaining.len() > LOOKAHEAD || (complete && remaining.len() > TARGET + PADDING))
                    .then(|| quiet_boundary(remaining))
            });
            let Some(cut) = cut else { break };

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
        if remaining.iter().any(|sample| *sample != 0.0) {
            let part = Zeroizing::new(recognize(remaining)?);
            append(&mut result, &part)?;
        }
        Ok(result.as_str().to_owned())
    }
}

// Qualification experiment: an automatic-language session can confirm an
// earlier phrase at a sustained relative pause. No samples are discarded and
// quiet speech still reaches inference. A two-second observed tail keeps the
// earliest cut stable across subsequently appended audio.
fn pause_boundary(samples: &[f32]) -> Option<usize> {
    const STEP: usize = SAMPLE_RATE / 10;
    const MIN_WINDOWS: usize = 40;
    const QUIET_WINDOWS: usize = 5;
    let bounded = samples.get(..samples.len().min(LOOKAHEAD))?;
    let energy = Zeroizing::new(
        bounded
            .chunks_exact(STEP)
            .map(|window| window.iter().map(|s| f64::from(*s).powi(2)).sum::<f64>())
            .collect::<Vec<_>>(),
    );
    let mut quiet_start = None;
    let mut reference = 0.0;
    for (index, current) in energy.iter().copied().enumerate().skip(MIN_WINDOWS) {
        if quiet_start.is_none() {
            reference = energy
                .iter()
                .skip(index.saturating_sub(10))
                .take(10)
                .sum::<f64>()
                / 10.0;
        }
        if reference > 0.0 && current * 32.0 <= reference {
            let first = *quiet_start.get_or_insert(index);
            if index.saturating_sub(first).saturating_add(1) >= QUIET_WINDOWS {
                let cut = first.checked_add(QUIET_WINDOWS / 2)?.checked_mul(STEP)?;
                if samples.len().saturating_sub(cut) >= INITIAL_DRAFT_SAMPLES {
                    return Some(cut);
                }
            }
        } else {
            quiet_start = None;
        }
    }
    None
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
    fn acoustic_admission_defers_noise_without_caching_or_losing_original_speech() {
        let noise = vec![0.0001; 3 * SAMPLE_RATE];
        let mut recognition = SpeechRecognition::default();
        for complete in [false, true] {
            let result = recognition
                .recognize_with_admission(
                    &noise,
                    complete,
                    false,
                    AcousticAdmission::Inactive,
                    |_| panic!("noise reached decoder"),
                )
                .unwrap();
            assert_eq!(result, "");
            assert!(recognition.draft_samples.is_none());
        }
        let mut recognition = SpeechRecognition::default();
        let mut speech = noise.clone();
        speech.extend(vec![0.01; SAMPLE_RATE / 2]);
        assert_eq!(
            recognition
                .recognize_with_admission(
                    &speech,
                    false,
                    false,
                    AcousticAdmission::Speech(noise.len()),
                    |_| panic!("preview lacks onset context")
                )
                .unwrap(),
            ""
        );
        assert!(recognition.draft_samples.is_none());
        assert_eq!(
            recognition
                .recognize_with_admission(
                    &speech,
                    true,
                    false,
                    AcousticAdmission::Speech(noise.len()),
                    |original| {
                        assert_eq!(original, speech);
                        Ok("spoken word".into())
                    }
                )
                .unwrap(),
            "spoken word"
        );
    }

    #[test]
    fn automatic_pause_keeps_the_confirmed_phrase_and_all_tail_audio() {
        let mut audio = vec![0.5; 12 * SAMPLE_RATE];
        audio[6 * SAMPLE_RATE..7 * SAMPLE_RATE].fill(0.0);
        let mut recognition = SpeechRecognition::default();
        let mut lengths = Vec::new();
        let first = recognition
            .recognize_with_policy(&audio[..9 * SAMPLE_RATE], false, true, |chunk| {
                lengths.push(chunk.len());
                Ok(if lengths.len() == 1 {
                    "English"
                } else {
                    "Deutsch"
                }
                .into())
            })
            .unwrap();
        assert_eq!(first, "English Deutsch");
        let consumed = recognition.consumed;
        assert!((6 * SAMPLE_RATE..7 * SAMPLE_RATE).contains(&consumed));
        assert_eq!(lengths.iter().sum::<usize>(), 9 * SAMPLE_RATE);
        lengths.clear();
        let final_text = recognition
            .recognize_with_policy(&audio, true, true, |chunk| {
                lengths.push(chunk.len());
                Ok("Deutsch final".into())
            })
            .unwrap();
        assert_eq!(final_text, "English Deutsch final");
        assert_eq!(lengths, [audio.len() - consumed]);
        assert!(
            recognition.retired && recognition.confirmed.is_empty() && recognition.draft.is_empty()
        );
    }

    #[test]
    fn automatic_pause_does_not_discard_constant_quiet_speech_or_decode_a_zero_tail() {
        let quiet = vec![f32::MIN_POSITIVE; 9 * SAMPLE_RATE];
        let mut recognition = SpeechRecognition::default();
        let mut calls = 0;
        assert_eq!(
            recognition
                .recognize_with_policy(&quiet, true, true, |chunk| {
                    assert_eq!(chunk, quiet);
                    calls += 1;
                    Ok("quiet speech".into())
                })
                .unwrap(),
            "quiet speech"
        );
        assert_eq!(calls, 1);
        let mut audio = vec![0.0; 12 * SAMPLE_RATE];
        audio[..6 * SAMPLE_RATE].fill(0.5);
        let mut recognition = SpeechRecognition::default();
        let mut calls = 0;
        assert_eq!(
            recognition
                .recognize_with_policy(&audio, true, true, |chunk| {
                    assert!(
                        chunk.iter().any(|sample| *sample != 0.0),
                        "exact silence must never infer"
                    );
                    calls += 1;
                    Ok("phrase".into())
                })
                .unwrap(),
            "phrase"
        );
        assert_eq!(calls, 1);
    }

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
    fn interrupted_tail_keeps_only_completed_canonical_prefix_and_redecodes_remainder() {
        let mut recognition = SpeechRecognition::default();
        let opening = vec![0.25; INITIAL_DRAFT_SAMPLES];
        assert_eq!(
            recognition
                .recognize_with(&opening, false, |_| Ok("earlier preview".into()))
                .unwrap(),
            "earlier preview"
        );
        let samples = vec![0.25; LOOKAHEAD + SAMPLE_RATE];
        let mut calls = 0;
        let mut canonical = Vec::new();
        let result = recognition.recognize_with(&samples, false, |chunk| {
            calls += 1;
            if calls == 1 {
                canonical.extend_from_slice(chunk);
                Ok("completed canonical prefix".into())
            } else {
                Err(SpeechError::DraftInterrupted)
            }
        });
        assert_eq!(result, Err(SpeechError::DraftInterrupted));
        assert_eq!(calls, 2);
        assert_eq!(recognition.consumed, canonical.len());
        assert_eq!(recognition.observed, samples.len());
        assert_eq!(recognition.confirmed.as_str(), "completed canonical prefix");
        assert!(
            recognition.draft.is_empty()
                && recognition.draft_samples.is_none()
                && !recognition.retired
        );
        let mut final_pcm = Vec::new();
        let final_text = recognition
            .recognize_with(&samples, true, |chunk| {
                final_pcm.extend_from_slice(chunk);
                Ok("fresh remaining text".into())
            })
            .unwrap();
        assert_eq!([canonical, final_pcm].concat(), samples);
        assert_eq!(
            final_text,
            "completed canonical prefix fresh remaining text"
        );
        assert!(
            recognition.retired && recognition.confirmed.is_empty() && recognition.draft.is_empty()
        );
    }

    #[test]
    fn interrupted_opening_has_no_final_cache_and_final_receives_all_pcm() {
        let mut recognition = SpeechRecognition::default();
        let samples = vec![0.25; INITIAL_DRAFT_SAMPLES + SAMPLE_RATE];
        assert_eq!(
            recognition.recognize_with(&samples, false, |_| Err(SpeechError::DraftInterrupted)),
            Err(SpeechError::DraftInterrupted)
        );
        assert_eq!(recognition.consumed, 0);
        assert!(
            recognition.confirmed.is_empty()
                && recognition.draft_samples.is_none()
                && !recognition.retired
        );
        let mut final_pcm = Vec::new();
        assert_eq!(
            recognition
                .recognize_with(&samples, true, |chunk| {
                    final_pcm.extend_from_slice(chunk);
                    Ok("whole final".into())
                })
                .unwrap(),
            "whole final"
        );
        assert_eq!(final_pcm, samples);
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
