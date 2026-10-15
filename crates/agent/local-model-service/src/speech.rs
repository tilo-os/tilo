//! Receipt-checked local speech and the worker's bounded PCM protocol.
//!
//! The native engine is for the owned worker process. The compositor uses
//! its pipe protocol; it never calls the C decoder or opens a model itself.
#[cfg(feature = "whisper")]
use std::ffi::{CString, c_char, c_void};
use std::fs::{self, OpenOptions};
use std::io::Read as _;
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::Path;
#[cfg(feature = "whisper")]
use std::ptr::NonNull;

use zeroize::Zeroizing;

use crate::catalog::VerifiedCatalog;
use crate::{InProcessCapability, installed_in_process_model};

#[cfg(feature = "whisper")]
pub mod activity;
#[cfg(feature = "whistle-aot")]
mod aot;
mod model;
#[cfg(feature = "whisper")]
mod recognition;
pub use model::SealedSpeechModel;
pub mod worker;
#[cfg(feature = "whisper")]
pub use recognition::SpeechRecognition;

pub use agent_provider_config::speech::{
    DraftPolicy, FRAME, MAX_CONFIG_BYTES, MAX_SAMPLES, MAX_TEXT_BYTES, RequestKind, SAMPLE_RATE,
    SpeechConfig, SpeechError, SpeechReply, request,
};
const MAX_MODEL_BYTES: u64 = 256 * 1024 * 1024;

/// Recheck a selected installed object against the actual trusted catalogue
/// and license record. Size and digest are checked on the same open file whose
/// bytes the decoder receives, avoiding a path-replacement gap.
/// # Errors
/// Rejects missing, unsigned, unaccepted, changed, oversized or unsafe objects.
pub fn speech_model_file(
    root: &Path,
    catalog: &VerifiedCatalog,
) -> Result<SealedSpeechModel, SpeechError> {
    model_file(root, catalog, InProcessCapability::SpeechRecognition)
}

/// Require a separately receipt-checked acoustic graph when the signed image
/// catalogue declares one. Legacy catalogues remain recognizer-only.
/// # Errors
/// A declared but uninstalled, changed or unaccepted graph fails before Ready.
pub fn speech_activity_file(
    root: &Path,
    catalog: &VerifiedCatalog,
) -> Result<Option<SealedSpeechModel>, SpeechError> {
    if !catalog.payload().models.iter().any(|model| {
        model.artifact_format == agent_provider_config::local::ModelArtifactFormat::SileroGgml
    }) {
        return Ok(None);
    }
    model_file(root, catalog, InProcessCapability::SpeechActivity).map(Some)
}

fn model_file(
    root: &Path,
    catalog: &VerifiedCatalog,
    capability: InProcessCapability,
) -> Result<SealedSpeechModel, SpeechError> {
    let metadata = fs::symlink_metadata(root).map_err(|_| SpeechError::ModelUnavailable)?;
    let uid = rustix::process::geteuid().as_raw();
    if !metadata.is_dir() || metadata.uid() != uid || metadata.permissions().mode() & 0o022 != 0 {
        return Err(SpeechError::ModelUnavailable);
    }
    let installed = installed_in_process_model(root, capability)
        .map_err(|_| SpeechError::ModelUnavailable)?
        .ok_or(SpeechError::ModelUnavailable)?;
    let revision = &installed.revision;
    let release = catalog
        .payload()
        .models
        .iter()
        .find(|model| {
            model.id == installed.id
                && &model.version == revision
                && model.in_process_capabilities().contains(&capability)
                && model.artifact.sha256 == installed.sha256
                && model.license_id == installed.license_id
        })
        .ok_or(SpeechError::ModelUnavailable)?;
    let state = crate::load_state(&root.join(crate::STATE_FILE))
        .map_err(|_| SpeechError::ModelUnavailable)?;
    if release.requires_acceptance
        && !state
            .accepted_licenses
            .get(&release.id)
            .is_some_and(|accepted| {
                accepted.model_version == release.version
                    && accepted.model_sha256 == release.artifact.sha256
                    && accepted.license_id == release.license_id
            })
    {
        return Err(SpeechError::ModelUnavailable);
    }
    if release.artifact.size_bytes > MAX_MODEL_BYTES {
        return Err(SpeechError::ModelUnavailable);
    }
    if capability == InProcessCapability::SpeechActivity
        && release.artifact.size_bytes > 4 * 1024 * 1024
    {
        return Err(SpeechError::ModelUnavailable);
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&installed.path)
        .map_err(|_| SpeechError::ModelReceipt)?;
    let metadata = file.metadata().map_err(|_| SpeechError::ModelReceipt)?;
    if !metadata.is_file() || metadata.len() != release.artifact.size_bytes {
        return Err(SpeechError::ModelReceipt);
    }
    model::seal_verified(
        file,
        release.artifact.size_bytes,
        &release.artifact.sha256,
        installed.id,
        installed.revision,
    )
    .map(|sealed| sealed.with_format(release.artifact_format))
}

/// Compatibility bytes for existing qualification callers. Production loads
/// the immutable descriptor directly, without a whole-model input allocation.
/// # Errors
/// Rejects the same catalogue, licence and object failures as `speech_model_file`.
pub fn speech_model_bytes(
    root: &Path,
    catalog: &VerifiedCatalog,
) -> Result<(String, String, Zeroizing<Vec<u8>>), SpeechError> {
    let model = speech_model_file(root, catalog)?;
    let mut bytes = Zeroizing::new(Vec::new());
    bytes
        .try_reserve_exact(usize::try_from(model.size()).map_err(|_| SpeechError::ModelReceipt)?)
        .map_err(|_| SpeechError::ModelUnavailable)?;
    model
        .file()
        .read_to_end(&mut bytes)
        .map_err(|_| SpeechError::ModelReceipt)?;
    Ok((model.id().to_owned(), model.revision().to_owned(), bytes))
}

/// Apply the system parser sandbox before loading untrusted native weights.
/// # Errors
/// Fails closed if full Landlock or seccomp enforcement is unavailable.
pub fn confine_worker() -> Result<(), SpeechError> {
    use sandbox_core::{landlock, seccomp};
    let policy = landlock::RulesetSpec::application(Vec::new(), landlock::NetRestriction::DenyAll);
    let status =
        landlock::restrict_self(&policy, |_, _| {}).map_err(|_| SpeechError::WorkerStopped)?;
    if landlock::outcome_from_status(&status.ruleset) != landlock::LandlockOutcome::FullyEnforced {
        return Err(SpeechError::WorkerStopped);
    }
    // Reuse the system's parser policy and pthread clone3 compatibility;
    // models/audio cannot create sockets, execute programs or gain privileges.
    seccomp::install(&seccomp::clone3_compat_program().map_err(|_| SpeechError::WorkerStopped)?)
        .map_err(|_| SpeechError::WorkerStopped)?;
    seccomp::install(&seccomp::parser_program().map_err(|_| SpeechError::WorkerStopped)?)
        .map_err(|_| SpeechError::WorkerStopped)
}

#[cfg(feature = "whisper")]
unsafe extern "C" {
    fn tilo_speech_load_fd(fd: i32, length: usize) -> *mut c_void;
    fn tilo_speech_load(bytes: *mut u8, length: usize) -> *mut c_void;
    fn tilo_speech_ready(context: *mut c_void) -> bool;
    fn tilo_speech_threads(context: *mut c_void, threads: i32) -> bool;
    fn tilo_speech_flash_attention(context: *mut c_void) -> bool;
    fn tilo_speech_vocabulary(context: *mut c_void, text: *const c_char, length: usize) -> bool;
    fn tilo_speech_free(context: *mut c_void);
    fn tilo_speech_accepts(context: *mut c_void, language: *const c_char) -> bool;
    fn tilo_speech_enable_draft_interrupt() -> bool;
    fn tilo_speech_draft_mode(context: *mut c_void, enabled: bool);
    fn tilo_speech_transcribe(
        context: *mut c_void,
        samples: *const f32,
        count: i32,
        language: *const c_char,
        output: *mut u8,
        capacity: usize,
    ) -> i32;
}

/// An exclusively owned CPU decoder. Its opaque pointer is neither Send nor
/// Sync. Keep this in the worker; parent cancellation terminates that owned
/// process even while native inference is busy. No Rust callback is installed.
#[cfg(feature = "whisper")]
pub struct SpeechEngine(NonNull<c_void>);
#[cfg(feature = "whisper")]
impl SpeechEngine {
    /// Install the owned worker's lock-free provisional-inference stop signal.
    /// Call before Ready; failure never advertises support or opens capture.
    /// # Errors
    /// Fails if the private signal handler or signal mask cannot be installed.
    pub fn enable_draft_interrupt() -> Result<(), SpeechError> {
        // SAFETY: called once in the owned worker; the native signal handler
        // only stores a lock-free atomic flag and never touches Rust state.
        if unsafe { tilo_speech_enable_draft_interrupt() } {
            Ok(())
        } else {
            Err(SpeechError::WorkerStopped)
        }
    }

    /// Mark only the synchronous draft call as eligible for interruption.
    pub fn draft_mode(&mut self, enabled: bool) {
        // SAFETY: this owner changes the live context only between native
        // calls. Final inference always clears this mode before decoding.
        unsafe { tilo_speech_draft_mode(self.0.as_ptr(), enabled) };
    }

    /// Observe the attention strategy actually passed to the loaded native
    /// engine. Qualification must not infer this from filenames or timings.
    pub fn flash_attention_enabled(&self) -> bool {
        // SAFETY: the exclusively owned live context returns one metadata
        // boolean; no model/audio pointers or text cross this query.
        unsafe { tilo_speech_flash_attention(self.0.as_ptr()) }
    }

    /// Configure bounded native vocabulary tokens without rewriting output.
    /// Set before starting a new recognition session. Failed replacement clears
    /// the old binding; do not reuse a recognition draft cache across changes.
    /// # Errors
    /// Invalid hints, oversized input or native tokenizer failure.
    pub fn set_vocabulary(&mut self, hints: &[String]) -> Result<(), SpeechError> {
        // SAFETY: the exclusively owned live context synchronously wipes its
        // token array; the static NUL string is valid even for empty input.
        if !unsafe { tilo_speech_vocabulary(self.0.as_ptr(), c"".as_ptr(), 0) } {
            return Err(SpeechError::Protocol);
        }
        if hints.len() > 256 {
            return Err(SpeechError::Protocol);
        }
        let mut text = Zeroizing::new(String::new());
        for hint in hints {
            if hint.is_empty()
                || hint.chars().count() > 128
                || !hint.chars().any(char::is_alphabetic)
                || hint.chars().any(|c| c.is_control() || c.is_whitespace())
                || hint.contains("<|")
                || hint.contains("|>")
            {
                return Err(SpeechError::Protocol);
            }
            if text.len().saturating_add(hint.len()).saturating_add(1) > 16 * 1024 {
                return Err(SpeechError::Protocol);
            }
            text.push_str(hint);
            text.push('\n');
        }
        let length = text.len();
        text.push('\0');
        // SAFETY: bounded, NUL-terminated UTF-8 remains live for this synchronous
        // call. The native context stores only its bounded owned token array.
        if unsafe { tilo_speech_vocabulary(self.0.as_ptr(), text.as_ptr().cast(), length) } {
            Ok(())
        } else {
            Err(SpeechError::Protocol)
        }
    }
    /// Parse the verified immutable descriptor after worker confinement.
    /// No native file path or Rust callback crosses this boundary.
    /// # Errors
    /// Rejects invalid headers, a changed seal or failed native loading.
    pub fn load_file(model: &SealedSpeechModel) -> Result<Self, SpeechError> {
        use std::os::fd::AsRawFd as _;
        use std::os::unix::fs::FileExt as _;
        model.check_seals()?;
        let mut magic = [0_u8; 4];
        if model.size() < 64
            || model.file().read_exact_at(&mut magic, 0).is_err()
            || &magic != b"lmgg"
        {
            return Err(SpeechError::ModelReceipt);
        }
        let length = usize::try_from(model.size()).map_err(|_| SpeechError::ModelReceipt)?;
        // SAFETY: the sealed descriptor remains live during synchronous C++
        // reads; native code retains neither descriptor nor callback userdata.
        let pointer = unsafe { tilo_speech_load_fd(model.file().as_raw_fd(), length) };
        NonNull::new(pointer)
            .map(Self)
            .ok_or(SpeechError::Recognition)
    }

    /// Load already receipt-checked model bytes synchronously in the worker.
    /// # Errors
    /// Rejects invalid headers, oversized models or failed native loading.
    pub fn load(bytes: &mut [u8]) -> Result<Self, SpeechError> {
        if bytes.len() < 64
            || u64::try_from(bytes.len()).map_or(true, |len| len > MAX_MODEL_BYTES)
            || !bytes.starts_with(b"lmgg")
        {
            return Err(SpeechError::ModelReceipt);
        }
        // SAFETY: the exclusive slice remains alive throughout synchronous
        // loading. The C++ loader copies weights and retains no input pointer.
        let pointer = unsafe { tilo_speech_load(bytes.as_mut_ptr(), bytes.len()) };
        NonNull::new(pointer)
            .map(Self)
            .ok_or(SpeechError::Recognition)
    }
    /// Match the worker's one- or two-CPU budget without oversubscribing it.
    /// Query the available CPUs before confinement, then pass only this count.
    /// # Errors
    /// Rejects any count outside the existing two-thread maximum.
    pub fn set_threads(&mut self, threads: usize) -> Result<(), SpeechError> {
        let threads = i32::try_from(threads).map_err(|_| SpeechError::Protocol)?;
        // SAFETY: the exclusive context is live; this stores only a bounded
        // integer and creates no threads or retained pointers.
        if unsafe { tilo_speech_threads(self.0.as_ptr(), threads) } {
            Ok(())
        } else {
            Err(SpeechError::Protocol)
        }
    }
    /// Allocate CPU decoder arenas after releasing the verified input bytes.
    /// Call before announcing readiness or opening a microphone.
    /// # Errors
    /// Rejects failed native allocation under the existing worker limit.
    pub fn prepare(&mut self) -> Result<(), SpeechError> {
        // SAFETY: this exclusively owned handle stays live throughout the
        // synchronous arena allocation; no input pointers are retained.
        if unsafe { tilo_speech_ready(self.0.as_ptr()) } {
            Ok(())
        } else {
            Err(SpeechError::Recognition)
        }
    }
    /// Confirm engine language support before opening a microphone.
    /// # Errors
    /// Rejects invalid/unsupported language codes; auto never translates.
    pub fn language(&self, language: &str) -> Result<CString, SpeechError> {
        if language.is_empty()
            || language.len() > 16
            || !language.bytes().all(|byte| byte.is_ascii_lowercase())
        {
            return Err(SpeechError::UnsupportedLanguage);
        }
        let language = CString::new(language).map_err(|_| SpeechError::UnsupportedLanguage)?;
        // SAFETY: both pointers are live for the synchronous metadata query;
        // context ownership excludes concurrent inference or destruction.
        if unsafe { tilo_speech_accepts(self.0.as_ptr(), language.as_ptr()) } {
            Ok(language)
        } else {
            Err(SpeechError::UnsupportedLanguage)
        }
    }
    /// Decode 16 kHz mono samples, replacing a draft rather than appending
    /// competing hypotheses. Caller supplies complete bounded phrase audio.
    /// # Errors
    /// Fails on oversized/invalid audio, native failure or invalid text.
    pub fn transcribe(
        &mut self,
        samples: &[f32],
        language: &CString,
    ) -> Result<String, SpeechError> {
        if samples.len() > MAX_SAMPLES || samples.iter().any(|sample| !sample.is_finite()) {
            return Err(SpeechError::AudioTooLong);
        }
        if samples.len() < SAMPLE_RATE / 4 || samples.iter().all(|sample| *sample == 0.0) {
            return Ok(String::new());
        }
        let count = i32::try_from(samples.len()).map_err(|_| SpeechError::AudioTooLong)?;
        let mut output = Zeroizing::new(vec![0; MAX_TEXT_BYTES]);
        // SAFETY: all slices and the NUL-terminated language live throughout
        // the synchronous call; capacity exactly bounds every C++ output write.
        // Params stay in C++, no foreign struct layout or callbacks cross FFI.
        let result = unsafe {
            tilo_speech_transcribe(
                self.0.as_ptr(),
                samples.as_ptr(),
                count,
                language.as_ptr(),
                output.as_mut_ptr(),
                output.len(),
            )
        };
        let length = usize::try_from(result).map_err(|_| match result {
            -2 => SpeechError::UnsupportedLanguage,
            -4 => SpeechError::TextTooLong,
            -5 => SpeechError::DraftInterrupted,
            _ => SpeechError::Recognition,
        })?;
        let text = std::str::from_utf8(output.get(..length).ok_or(SpeechError::TextTooLong)?)
            .map_err(|_| SpeechError::Recognition)?;
        // ASR segment whitespace is not a spoken Enter/paragraph command.
        // Normalize whitespace before the compositor's control-character gate.
        if text
            .chars()
            .any(|ch| ch.is_control() && !ch.is_whitespace())
        {
            return Err(SpeechError::Recognition);
        }
        Ok(text.split_whitespace().collect::<Vec<_>>().join(" "))
    }
}
#[cfg(feature = "whisper")]
impl Drop for SpeechEngine {
    fn drop(&mut self) {
        // SAFETY: exactly this owner releases this live context exactly once.
        unsafe { tilo_speech_free(self.0.as_ptr()) };
    }
}

pub struct SpeechInput {
    sequence: u64,
    finished: bool,
    samples: Zeroizing<Vec<f32>>,
}
impl Default for SpeechInput {
    fn default() -> Self {
        Self {
            sequence: 0,
            finished: false,
            samples: Zeroizing::new(Vec::new()),
        }
    }
}
impl SpeechInput {
    /// Parse one real framed request and reject replay, gaps and discontinuity.
    /// # Errors
    /// Rejects malformed headers, sequences, PCM or unexpected control payloads.
    pub fn accept<'a>(&mut self, frame: &'a [u8]) -> Result<(RequestKind, &'a [u8]), SpeechError> {
        let sequence = u64::from_le_bytes(
            frame
                .get(1..9)
                .and_then(|s| s.try_into().ok())
                .ok_or(SpeechError::Protocol)?,
        );
        if self.finished || self.sequence.checked_add(1) != Some(sequence) {
            return Err(SpeechError::Protocol);
        }
        let payload = frame.get(9..).ok_or(SpeechError::Protocol)?;
        let kind = match frame.first() {
            Some(0) if self.sequence == 0 => RequestKind::Configure,
            Some(1) if self.sequence > 0 => RequestKind::Audio,
            Some(2) if self.sequence > 0 => RequestKind::Draft,
            Some(3) if self.sequence > 0 => RequestKind::Finish,
            _ => return Err(SpeechError::Protocol),
        };
        if kind == RequestKind::Audio {
            if payload.is_empty()
                || payload.len() > 64 * 1024
                || payload.len().checked_rem(2) != Some(0)
            {
                return Err(SpeechError::Protocol);
            }
            if self.samples.len().saturating_add(payload.len() / 2) > MAX_SAMPLES {
                return Err(SpeechError::AudioTooLong);
            }
            self.samples.extend(
                payload
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|pair| f32::from(i16::from_le_bytes(*pair)) / 32768.0),
            );
        } else if (kind != RequestKind::Configure && !payload.is_empty())
            || (kind == RequestKind::Configure && payload.len() > MAX_CONFIG_BYTES)
        {
            return Err(SpeechError::Protocol);
        }
        self.sequence = sequence;
        self.finished = kind == RequestKind::Finish;
        Ok((kind, payload))
    }
    pub fn samples(&self) -> &[f32] {
        &self.samples
    }
    pub fn sequence(&self) -> u64 {
        self.sequence
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn configured() -> Result<SpeechInput, SpeechError> {
        let mut input = SpeechInput::default();
        input.accept(&request(RequestKind::Configure, 1, br#"{"language":"en"}"#))?;
        Ok(input)
    }

    #[test]
    fn private_stream_preserves_pcm_and_finishes_once() {
        let mut input = configured().expect("valid configuration");
        input
            .accept(&request(RequestKind::Audio, 2, &[0, 128, 255, 127, 0, 0]))
            .expect("valid PCM");
        assert_eq!(input.samples(), &[-1.0, 32767.0 / 32768.0, 0.0]);
        input
            .accept(&request(RequestKind::Draft, 3, &[]))
            .expect("draft");
        assert_eq!(input.samples().len(), 3);
        input
            .accept(&request(RequestKind::Finish, 4, &[]))
            .expect("finish");
        assert!(matches!(
            input.accept(&request(RequestKind::Audio, 5, &[0, 0])),
            Err(SpeechError::Protocol)
        ));
    }

    #[test]
    fn gaps_replay_malformed_audio_and_control_payloads_fail() {
        let mut input = configured().expect("valid configuration");
        for bytes in [
            request(RequestKind::Audio, 3, &[0, 0]),
            request(RequestKind::Audio, 1, &[0, 0]),
            request(RequestKind::Audio, 2, &[0]),
            request(RequestKind::Audio, 2, &[]),
            request(RequestKind::Draft, 2, &[0]),
            request(RequestKind::Configure, 2, &[]),
            vec![1],
        ] {
            assert!(matches!(input.accept(&bytes), Err(SpeechError::Protocol)));
            assert_eq!(input.sequence(), 1);
            assert!(input.samples().is_empty());
        }
    }

    #[test]
    fn the_real_pcm_parser_enforces_the_session_budget() {
        let mut input = configured().expect("valid configuration");
        let chunk = vec![0; 64 * 1024];
        while input.samples().len() + chunk.len() / 2 <= MAX_SAMPLES {
            input
                .accept(&request(RequestKind::Audio, input.sequence() + 1, &chunk))
                .expect("bounded audio");
        }
        let remaining = MAX_SAMPLES - input.samples().len();
        input
            .accept(&request(
                RequestKind::Audio,
                input.sequence() + 1,
                &vec![0; remaining * 2],
            ))
            .expect("bounded audio");
        let before = input.sequence();
        assert!(matches!(
            input.accept(&request(RequestKind::Audio, before + 1, &[0, 0])),
            Err(SpeechError::AudioTooLong)
        ));
        assert_eq!(input.sequence(), before);
        assert_eq!(input.samples().len(), MAX_SAMPLES);
    }

    #[cfg(feature = "whisper")]
    #[test]
    fn short_or_invalid_model_headers_never_reach_the_native_parser() {
        for mut bytes in [Vec::new(), vec![0; 64], b"lmgg".to_vec()] {
            assert!(matches!(
                SpeechEngine::load(&mut bytes),
                Err(SpeechError::ModelReceipt)
            ));
        }
    }
}
