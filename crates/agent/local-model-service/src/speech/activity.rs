//! Independent acoustic evidence; analysis never changes recognition PCM.
use super::{MAX_SAMPLES, SpeechError};

unsafe extern "C" {
    fn tilo_activity_load_fd(fd: i32, length: usize) -> *mut std::ffi::c_void;
    fn tilo_activity_free(context: *mut std::ffi::c_void);
    fn tilo_activity_observe(
        context: *mut std::ffi::c_void,
        samples: *const f32,
        count: i32,
        normalize: bool,
        counters: *mut u64,
        probabilities: *mut f32,
    ) -> bool;
    fn tilo_silero_activity(
        model: *const u8,
        length: usize,
        samples: *const f32,
        count: i32,
        normalize: bool,
        counters: *mut u64,
        probabilities: *mut f32,
    ) -> bool;
}

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
        || samples.is_empty()
        || samples.len() > MAX_SAMPLES
        || samples
            .iter()
            .any(|sample| !sample.is_finite() || sample.abs() > 1.0)
    {
        return Err(SpeechError::Protocol);
    }
    let count = i32::try_from(samples.len()).map_err(|_| SpeechError::Protocol)?;
    let mut counters = 0;
    let mut probabilities = [0.0_f32; 4];
    // SAFETY: verified bounded model bytes and finite PCM remain live during
    // the synchronous parser. Output buffers hold one u64 and exactly four f32s.
    if !unsafe {
        tilo_silero_activity(
            model.as_ptr(),
            model.len(),
            samples.as_ptr(),
            count,
            normalize,
            &raw mut counters,
            probabilities.as_mut_ptr(),
        )
    } {
        return Err(SpeechError::Recognition);
    }
    Ok(SileroEvidence {
        frames: unpack(counters),
        min_probability: probabilities[0],
        max_probability: probabilities[1],
        mean_probability: probabilities[2],
        total_frames: probabilities[3],
    })
}

fn unpack(counters: u64) -> ActivityEvidence {
    let [
        positive_frames,
        first_positive_frame,
        last_positive_frame,
        longest_positive_run,
    ] = [0, 16, 32, 48]
        .map(|shift| u16::try_from((counters >> shift) & 0xffff).unwrap_or_default());
    ActivityEvidence {
        positive_frames,
        first_positive_frame,
        last_positive_frame,
        longest_positive_run,
    }
}

/// Exclusive CPU graph owned by one confined speech worker.
pub struct SpeechActivity {
    context: std::ptr::NonNull<std::ffi::c_void>,
    _exclusive: std::marker::PhantomData<std::rc::Rc<()>>,
    admitted_onset: Option<usize>,
}
impl Drop for SpeechActivity {
    fn drop(&mut self) {
        // SAFETY: this owner releases its exclusive context exactly once.
        unsafe { tilo_activity_free(self.context.as_ptr()) };
    }
}
impl SpeechActivity {
    /// Load only an immutable catalogue-verified graph after confinement.
    /// # Errors
    /// Rejects changed seals, wrong artifact types or failed native parsing.
    pub fn load_file(model: &super::SealedSpeechModel) -> Result<Self, SpeechError> {
        use std::os::fd::AsRawFd as _;
        model.check_seals()?;
        if model.format() != agent_provider_config::local::ModelArtifactFormat::SileroGgml
            || model.size() > 4 * 1024 * 1024
        {
            return Err(SpeechError::ModelReceipt);
        }
        let length = usize::try_from(model.size()).map_err(|_| SpeechError::ModelReceipt)?;
        // SAFETY: the sealed descriptor lives through synchronous native reads.
        let context = unsafe { tilo_activity_load_fd(model.file().as_raw_fd(), length) };
        Ok(Self {
            context: std::ptr::NonNull::new(context).ok_or(SpeechError::Recognition)?,
            _exclusive: std::marker::PhantomData,
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
        let count = i32::try_from(samples.len()).map_err(|_| SpeechError::Protocol)?;
        let mut first = None;
        for normalize in [false, true] {
            let mut counters = 0;
            let mut probabilities = [0.0_f32; 4];
            // SAFETY: the exclusive graph and bounded PCM/output buffers remain
            // live through synchronous inference. Native code retains no PCM.
            if !unsafe {
                tilo_activity_observe(
                    self.context.as_ptr(),
                    samples.as_ptr(),
                    count,
                    normalize,
                    &raw mut counters,
                    probabilities.as_mut_ptr(),
                )
            } {
                return Err(SpeechError::Recognition);
            }
            let evidence = unpack(counters);
            if evidence.positive_frames > 0 {
                let offset = usize::from(evidence.first_positive_frame.saturating_sub(1)) * 512;
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
