//! Independent acoustic evidence; analysis never changes recognition PCM.
use std::ptr::NonNull;

use whisper_rs_sys as sys;
use zeroize::Zeroizing;

use super::engine::{Cursor, Source};
use super::{MAX_SAMPLES, SpeechError};

/// Public counters, not speech text, ASR confidence or permission decisions.
#[derive(Debug)]
pub struct ActivityEvidence {
    pub positive_frames: u16,
    pub first_positive_frame: u16,
    pub last_positive_frame: u16,
    pub longest_positive_run: u16,
}

#[derive(Debug)]
pub struct SileroEvidence {
    pub frames: ActivityEvidence,
    pub min_probability: f32,
    pub max_probability: f32,
    pub mean_probability: f32,
    pub total_frames: f32,
}

/// Observe neural acoustic evidence from the pinned MIT review object.
/// This is not a trusted-catalogue loader and cannot activate a normal worker.
pub fn observe_silero(
    model: &[u8],
    samples: &[f32],
    normalize: bool,
) -> Result<SileroEvidence, SpeechError> {
    use sha2::Digest as _;
    if model.len() != 885_098
        || hex::encode(sha2::Sha256::digest(model))
            != "2aa269b785eeb53a82983a20501ddf7c1d9c48e33ab63a41391ac6c9f7fb6987"
    {
        return Err(SpeechError::Protocol);
    }
    let silero = Silero::load(&mut Cursor::new(Source::Bytes(model), model.len()))?;
    observe(silero.context(), samples, normalize)
}

/// Exclusive CPU graph owned by one confined speech worker.
pub struct SpeechActivity {
    silero: Silero,
    admitted_onset: Option<usize>,
}

impl SpeechActivity {
    /// Load only an immutable catalogue-verified graph after confinement.
    pub fn load_file(model: &super::SealedSpeechModel) -> Result<Self, SpeechError> {
        model.check_seals()?;
        if model.format() != agent_provider_config::local::ModelArtifactFormat::SileroGgml {
            return Err(SpeechError::ModelReceipt);
        }
        let length = usize::try_from(model.size()).map_err(|_| SpeechError::ModelReceipt)?;
        let silero = Silero::load(&mut Cursor::new(Source::Descriptor(model.file()), length))?;
        Ok(Self {
            silero,
            admitted_onset: None,
        })
    }

    /// Earliest acoustic activity in either original or normalized analysis.
    /// This changes neither recognition samples nor decoder confidence.
    pub fn first_sample(&mut self, samples: &[f32]) -> Result<Option<usize>, SpeechError> {
        if samples.len() > MAX_SAMPLES || samples.iter().any(|s| !s.is_finite() || s.abs() > 1.0) {
            return Err(SpeechError::Protocol);
        }
        if samples.is_empty() {
            return Ok(None);
        }
        let mut first = None;
        for normalize in [false, true] {
            let evidence = observe(self.silero.context(), samples, normalize)?;
            if evidence.frames.positive_frames > 0 {
                let offset = usize::from(evidence.frames.first_positive_frame.saturating_sub(1))
                    .saturating_mul(512);
                first = Some(first.map_or(offset, |old: usize| old.min(offset)));
            }
        }
        // A later louder tail changes the normalized analysis gain. It must
        // never revoke speech already observed on this append-only session.
        // Keep the current onset while it is measurable, preserving preview
        // context scheduling; otherwise retain the last admitted onset.
        if first.is_some() {
            self.admitted_onset = first;
        }
        Ok(first.or(self.admitted_onset))
    }
}

/// One loaded activity graph. It is neither `Send` nor `Sync`, and one owner
/// releases it exactly once.
struct Silero {
    context: NonNull<sys::whisper_vad_context>,
}

impl Silero {
    fn load(cursor: &mut Cursor<'_>) -> Result<Self, SpeechError> {
        if !(64..=4 * 1024 * 1024).contains(&cursor.length()) {
            return Err(SpeechError::ModelReceipt);
        }
        // SAFETY: the pinned defaults initialize every field.
        let mut params = unsafe { sys::whisper_vad_default_context_params() };
        params.use_gpu = false;
        // This small per-frame graph needs no second idle/spinning CPU pool.
        params.n_threads = 1;
        let mut loader = cursor.loader();
        // SAFETY: the loader and the bytes behind the cursor live through this
        // call; the parser copies the graph and retains neither.
        let context = unsafe { sys::whisper_vad_init_with_params(&raw mut loader, params) };
        Ok(Self {
            context: NonNull::new(context).ok_or(SpeechError::Recognition)?,
        })
    }

    fn context(&self) -> NonNull<sys::whisper_vad_context> {
        self.context
    }
}

impl Drop for Silero {
    fn drop(&mut self) {
        // SAFETY: exactly this owner releases this live graph exactly once.
        unsafe { sys::whisper_vad_free(self.context.as_ptr()) };
    }
}

fn observe(
    context: NonNull<sys::whisper_vad_context>,
    samples: &[f32],
    normalize: bool,
) -> Result<SileroEvidence, SpeechError> {
    if samples.is_empty()
        || samples.len() > MAX_SAMPLES
        || samples
            .iter()
            .any(|sample| !sample.is_finite() || sample.abs() > 1.0)
    {
        return Err(SpeechError::Protocol);
    }
    let count = i32::try_from(samples.len()).map_err(|_| SpeechError::Protocol)?;
    // A quiet onset is measured again at half gain; the original PCM the
    // recognizer receives is never rescaled.
    let peak = samples
        .iter()
        .fold(0.0_f32, |peak, sample| peak.max(sample.abs()));
    let normalized = (normalize && peak > 0.0).then(|| {
        Zeroizing::new(
            samples
                .iter()
                .map(|sample| sample * (0.5 / peak))
                .collect::<Vec<_>>(),
        )
    });
    let analyzed = normalized
        .as_deref()
        .map_or(samples, |normalized| normalized.as_slice());
    // SAFETY: the graph and finite PCM live through this synchronous call, and
    // the pinned API retains no PCM afterwards.
    if !unsafe { sys::whisper_vad_detect_speech(context.as_ptr(), analyzed.as_ptr(), count) } {
        return Err(SpeechError::Recognition);
    }
    // SAFETY: these are queries on the graph the call above just ran.
    let reported = unsafe { sys::whisper_vad_n_probs(context.as_ptr()) };
    let frames = usize::try_from(reported).map_err(|_| SpeechError::Recognition)?;
    let probabilities = NonNull::new(unsafe { sys::whisper_vad_probs(context.as_ptr()) })
        .ok_or(SpeechError::Recognition)?;
    if frames == 0 || frames > 6000 {
        return Err(SpeechError::Recognition);
    }
    // SAFETY: the graph owns exactly this many probabilities after the call.
    let probabilities = unsafe { std::slice::from_raw_parts(probabilities.as_ptr(), frames) };
    let mut evidence = ActivityEvidence {
        positive_frames: 0,
        first_positive_frame: 0,
        last_positive_frame: 0,
        longest_positive_run: 0,
    };
    let mut consecutive: u16 = 0;
    let mut minimum = 1.0_f32;
    let mut maximum = 0.0_f32;
    let mut total = 0.0_f32;
    for (index, probability) in probabilities.iter().enumerate() {
        if !probability.is_finite() || !(0.0..=1.0).contains(probability) {
            return Err(SpeechError::Recognition);
        }
        minimum = minimum.min(*probability);
        maximum = maximum.max(*probability);
        total += *probability;
        if *probability >= 0.5 {
            evidence.positive_frames = evidence.positive_frames.saturating_add(1);
            let frame = u16::try_from(index.saturating_add(1)).unwrap_or(u16::MAX);
            if evidence.first_positive_frame == 0 {
                evidence.first_positive_frame = frame;
            }
            evidence.last_positive_frame = frame;
            consecutive = consecutive.saturating_add(1);
            evidence.longest_positive_run = evidence.longest_positive_run.max(consecutive);
        } else {
            consecutive = 0;
        }
    }
    let frames = u16::try_from(probabilities.len()).map_err(|_| SpeechError::Recognition)?;
    Ok(SileroEvidence {
        frames: evidence,
        min_probability: minimum,
        max_probability: maximum,
        mean_probability: total / f32::from(frames),
        total_frames: f32::from(frames),
    })
}
