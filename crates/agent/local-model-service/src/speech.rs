//! Receipt-checked local speech and the worker's bounded PCM protocol.
//!
//! The decoder belongs to the owned worker process. The compositor uses its
//! pipe protocol; it never loads a model or decodes audio itself.
use std::fs::{self, OpenOptions};
use std::io::Read as _;
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::Path;

use zeroize::Zeroizing;

use crate::catalog::VerifiedCatalog;
use crate::{InProcessCapability, installed_in_process_model};

#[cfg(feature = "whisper")]
pub mod activity;
#[cfg(feature = "whistle-aot")]
mod aot;
#[cfg(feature = "whisper")]
mod engine;
mod model;
#[cfg(feature = "whisper")]
mod recognition;
pub use model::SealedSpeechModel;
pub mod worker;
#[cfg(feature = "whisper")]
pub use engine::SpeechEngine;
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
pub fn speech_model_file(
    root: &Path,
    catalog: &VerifiedCatalog,
) -> Result<SealedSpeechModel, SpeechError> {
    model_file(root, catalog, InProcessCapability::SpeechRecognition)
}

/// Require a separately receipt-checked acoustic graph when the signed image
/// catalogue declares one. Legacy catalogues remain recognizer-only.
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
        for bytes in [Vec::new(), vec![0; 64], b"lmgg".to_vec()] {
            assert!(matches!(
                SpeechEngine::load(&bytes),
                Err(SpeechError::ModelReceipt)
            ));
        }
    }
}
