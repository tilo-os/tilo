//! Receipt-checked local speech and the worker's bounded PCM protocol.
//!
//! The native engine is for the owned worker process. The compositor uses
//! its pipe protocol; it never calls the C decoder or opens a model itself.
use std::ffi::{CString, c_char, c_void};
use std::fs::{self, OpenOptions};
use std::io::Read as _;
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::Path;
use std::ptr::NonNull;

use agent_provider_config::local::ModelArtifactFormat;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use zeroize::Zeroizing;

use crate::catalog::VerifiedCatalog;
use crate::{InProcessCapability, installed_in_process_model};

pub mod worker;

pub const SAMPLE_RATE: usize = 16_000;
pub const MAX_SAMPLES: usize = SAMPLE_RATE * 120;
pub const MAX_TEXT_BYTES: usize = 16 * 1024;
pub const FRAME: wire_frame::Framing =
    wire_frame::Framing::little_endian(64 * 1024 + 9).reject_empty();
const MAX_MODEL_BYTES: u64 = 256 * 1024 * 1024;

/// Status contains no audio, transcript, path or model-controlled message.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpeechError {
    ModelUnavailable,
    ModelReceipt,
    UnsupportedLanguage,
    Protocol,
    AudioTooLong,
    TextTooLong,
    Recognition,
    WorkerStopped,
}

/// A reply to one monotonic request sequence on one private worker pipe.
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SpeechReply {
    Ready {
        sequence: u64,
        model: String,
        revision: String,
    },
    Draft {
        sequence: u64,
        text: String,
    },
    Done {
        sequence: u64,
        text: String,
    },
    Error {
        sequence: u64,
        error: SpeechError,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpeechConfig {
    pub language: String,
}

/// Recheck a selected installed object against the actual trusted catalogue
/// and license record. Size and digest are checked on the same open file whose
/// bytes the decoder receives, avoiding a path-replacement gap.
/// # Errors
/// Rejects missing, unsigned, unaccepted, changed, oversized or unsafe objects.
pub fn speech_model_bytes(
    root: &Path,
    catalog: &VerifiedCatalog,
) -> Result<(String, String, Zeroizing<Vec<u8>>), SpeechError> {
    let metadata = fs::symlink_metadata(root).map_err(|_| SpeechError::ModelUnavailable)?;
    // SAFETY: geteuid is argument-free and cannot fail.
    let uid = unsafe { libc::geteuid() };
    if !metadata.is_dir() || metadata.uid() != uid || metadata.permissions().mode() & 0o022 != 0 {
        return Err(SpeechError::ModelUnavailable);
    }
    let installed = installed_in_process_model(root, InProcessCapability::SpeechRecognition)
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
                && model.artifact_format == ModelArtifactFormat::WhisperGgml
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
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&installed.path)
        .map_err(|_| SpeechError::ModelReceipt)?;
    let metadata = file.metadata().map_err(|_| SpeechError::ModelReceipt)?;
    if !metadata.is_file() || metadata.len() != release.artifact.size_bytes {
        return Err(SpeechError::ModelReceipt);
    }
    let mut bytes = Zeroizing::new(Vec::new());
    (&mut file)
        .take(release.artifact.size_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|_| SpeechError::ModelReceipt)?;
    if u64::try_from(bytes.len()).ok() != Some(release.artifact.size_bytes)
        || hex::encode(Sha256::digest(&*bytes)) != release.artifact.sha256
    {
        return Err(SpeechError::ModelReceipt);
    }
    Ok((installed.id, installed.revision, bytes))
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

unsafe extern "C" {
    fn tilo_speech_load(bytes: *mut u8, length: usize) -> *mut c_void;
    fn tilo_speech_free(context: *mut c_void);
    fn tilo_speech_accepts(context: *mut c_void, language: *const c_char) -> bool;
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
pub struct SpeechEngine(NonNull<c_void>);
impl SpeechEngine {
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
impl Drop for SpeechEngine {
    fn drop(&mut self) {
        // SAFETY: exactly this owner releases this live context exactly once.
        unsafe { tilo_speech_free(self.0.as_ptr()) };
    }
}

/// Requests are ordered on a private byte pipe. Audio frames contain S16LE;
/// control frames never accept model paths, commands, URLs or arbitrary prompts.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RequestKind {
    Configure = 0,
    Audio = 1,
    Draft = 2,
    Finish = 3,
}

pub fn request(kind: RequestKind, sequence: u64, payload: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(payload.len().saturating_add(9));
    bytes.push(kind as u8);
    bytes.extend(sequence.to_le_bytes());
    bytes.extend(payload);
    bytes
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
            || (kind == RequestKind::Configure && payload.len() > 1024)
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
