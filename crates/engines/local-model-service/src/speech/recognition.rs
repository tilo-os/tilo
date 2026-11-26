//! Bounded native decoding with immutable confirmed audio prefixes.
use super::{MAX_SAMPLES, MAX_TEXT_BYTES, SAMPLE_RATE, SpeechEngine, SpeechError};
use std::ffi::CString;
use zeroize::{Zeroize as _, Zeroizing};

/// Independent acoustic admission, measured before a decode starts so the
/// serving loop can keep one activity graph while a provisional decode runs
/// elsewhere.
#[derive(Clone, Copy)]
pub(super) enum Admission {
    Legacy,
    Inactive,
    Speech(usize),
}

impl Admission {
    pub(super) fn measure(
        activity: Option<&mut super::activity::SpeechActivity>,
        samples: &[f32],
    ) -> Result<Self, SpeechError> {
        let Some(graph) = activity else {
            return Ok(Self::Legacy);
        };
        Ok(graph
            .first_sample(samples)?
            .map_or(Self::Inactive, Self::Speech))
    }
}

const TARGET: usize = 20 * SAMPLE_RATE;
const INITIAL_DRAFT_SAMPLES: usize = 2 * SAMPLE_RATE;
const PADDING: usize = TARGET / 4;
const QUIET_WINDOW: usize = SAMPLE_RATE / 10;
const LOOKAHEAD: usize = TARGET + PADDING + QUIET_WINDOW;

/// One worker session. Audio must be the append-only buffer of its `SpeechInput`.
/// Confirmed prefixes are decoded once; only the unfinished tail is revised.
/// Cancellation still terminates the owned worker, including native inference.
#[derive(Default, Clone)]
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
    pub fn transcribe(
        &mut self,
        engine: &mut SpeechEngine,
        samples: &[f32],
        language: &CString,
        complete: bool,
    ) -> Result<String, SpeechError> {
        self.transcribe_admitted(engine, Admission::Legacy, samples, language, complete)
    }

    /// Decode original PCM after independent acoustic admission. Final speech
    /// never waits for the preview context requirement and never loses samples.
    pub(super) fn transcribe_admitted(
        &mut self,
        engine: &mut SpeechEngine,
        admission: Admission,
        samples: &[f32],
        language: &CString,
        complete: bool,
    ) -> Result<String, SpeechError> {
        self.recognize_with_admission(
            samples,
            complete,
            language.to_bytes() == b"auto",
            admission,
            |chunk| engine.transcribe(chunk, language, !complete),
        )
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
        self.recognize_with_admission(samples, complete, automatic, Admission::Legacy, recognize)
    }

    fn recognize_with_admission(
        &mut self,
        samples: &[f32],
        complete: bool,
        automatic: bool,
        admission: Admission,
        recognize: impl FnMut(&[f32]) -> Result<String, SpeechError>,
    ) -> Result<String, SpeechError> {
        let no_activity = matches!(admission, Admission::Inactive);
        let context_start = match admission {
            Admission::Speech(first) => first,
            Admission::Legacy | Admission::Inactive => 0,
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
