//! Destination-bound protection for an interactive prepared setup.
//!
//! A package must not carry another computer's identity, tokens or file ciphertext protected only by a
//! login password chosen later. On the same computer the signed target boot artifact is already on the
//! live medium, so one machine-generated secret is bound to that exact target and this machine's TPM.
//!
//! The wire format lives in [`staging_handoff_model::prepared`], so both sides read one definition;
//! only this backend reaches the TPM, reads a caller-owned package, runs the privileged helper and
//! resolves `SUDO_UID`.
//!
//! The policy is smaller than a rollback token: a signed `PolicyPCR -> PolicyAuthorize -> PolicyPCR`
//! chain, not an unbound PCR0 blob.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use zeroize::Zeroizing;

pub use staging_handoff_model::prepared::{
    MAX_SIDECAR, MAX_SIDECAR_BYTES, MAX_SIGNATURE_BYTES, MAX_UKI, PCRS, PLATFORM_PCRS,
    PREDICTIONS_DIR, RECEIVER_UID, SCHEMA, SIDECAR_FILE, SealedPreparation, SignedTarget,
    TARGET_UKI_FILE, TargetPolicyBundle, b64, sha256, unb64,
};

/// The exact bundle directory it was reviewed from.
#[derive(Clone, Debug)]
pub struct TargetAcquisition {
    target: SignedTarget,
    bundle: PathBuf,
}

impl TargetAcquisition {
    pub(crate) fn new(target: SignedTarget, bundle: PathBuf) -> Self {
        Self { target, bundle }
    }

    pub fn target(&self) -> &SignedTarget {
        &self.target
    }

    pub fn generation(&self) -> u64 {
        self.target.generation()
    }

    pub fn target_uki_sha256(&self) -> [u8; 32] {
        self.target.target_uki_sha256()
    }

    pub fn bundle(&self) -> &Path {
        &self.bundle
    }
}

/// A stage a sealing request passes through, so the frontend keeps its own progress description while
/// the backend does the work.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SealStage {
    ReadingPackage,
    Sealing,
    WritingSidecar,
}

pub struct AcquireRequest<'a> {
    pub policy: Option<&'a TargetPolicyBundle>,
    pub cancel: &'a AtomicBool,
}

/// The secret is the machine-generated package password, never a person's login password.
pub struct SealPreparationRequest<'a> {
    pub target: &'a TargetAcquisition,
    pub package: &'a Path,
    pub sidecar_directory: &'a Path,
    pub source_public_key: [u8; 32],
    pub secret: &'a str,
    pub cancel: &'a AtomicBool,
    pub progress: &'a dyn Fn(SealStage) -> io::Result<()>,
}

/// Hex keeps the canonical carrier's 15-character policy satisfied while giving 256 bits of machine
/// randomness; it is never shown to the person and never a login password.
pub fn generate_secret() -> io::Result<Zeroizing<String>> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes).map_err(io::Error::other)?;
    Ok(Zeroizing::new(hex::encode(bytes)))
}

#[cfg(target_os = "linux")]
mod native;
#[cfg(target_os = "linux")]
pub use native::*;
#[cfg(not(target_os = "linux"))]
mod unsupported;
#[cfg(not(target_os = "linux"))]
pub use unsupported::{
    preflight_handoff, read_prepared_handoff, seal_prepared_handoff, target_for_preparation,
};

#[cfg(all(test, target_os = "linux"))]
mod tests;
