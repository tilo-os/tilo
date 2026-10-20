//! Destination-bound protection for an interactive prepared setup.
//!
//! The ordinary installation journey no longer asks for a preparation
//! passphrase. It still must not write another computer's identity, account
//! tokens or file ciphertext into a package protected only by a login password
//! chosen later. On the same computer, the target boot artifact is already
//! signed and present on the live medium, so this module binds one
//! machine-generated package secret to the exact signed target and this
//! machine's TPM.
//!
//! The public wire format — the signed target policy, the receiver identity and
//! the sealed blob — lives in [`staging_handoff_model::prepared`] so the
//! preparation side and the installed boot read one definition and neither can
//! accept a field the other refuses. What only this platform backend may do
//! stays here: reach this machine's TPM, read a caller-owned package, run the
//! vetted privileged helper and resolve the original `SUDO_UID`.
//!
//! This is deliberately a *smaller* policy than a rollback token: the target
//! NV floor does not exist yet at preparation time. It is still a signed
//! `PolicyPCR -> PolicyAuthorize -> PolicyPCR` chain, not an unbound PCR0
//! blob, and it refuses a target whose signed artifacts cannot be verified.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use zeroize::Zeroizing;

pub use staging_handoff_model::prepared::{
    MAX_SIDECAR, MAX_SIDECAR_BYTES, MAX_SIGNATURE_BYTES, MAX_UKI, PCRS, PLATFORM_PCRS,
    PREDICTIONS_DIR, RECEIVER_UID, SCHEMA, SIDECAR_FILE, SealedPreparation, SignedTarget,
    TARGET_UKI_FILE, TargetPolicyBundle, b64, sha256, unb64,
};

/// A target boot policy acquired for this same-computer preparation, and the
/// exact bundle directory it was reviewed from.
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

    /// The reviewed bundle a privileged helper re-reviews before sealing.
    pub fn bundle(&self) -> &Path {
        &self.bundle
    }
}

/// A stage a sealing request passes through, so the frontend keeps its own
/// progress description while the backend does the work.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SealStage {
    ReadingPackage,
    Sealing,
    WritingSidecar,
}

/// A request to resolve this same-computer preparation's target boot policy.
pub struct AcquireRequest<'a> {
    /// The release catalog's published policy for the target architecture, if
    /// it publishes one.
    pub policy: Option<&'a TargetPolicyBundle>,
    pub cancel: &'a AtomicBool,
}

/// Everything one seal needs, in one typed request. The secret is the
/// machine-generated package password and never a person's login password.
pub struct SealPreparationRequest<'a> {
    pub target: &'a TargetAcquisition,
    /// The canonical prepared package the secret belongs to.
    pub package: &'a Path,
    /// The private preparation directory the public sidecar is written beside.
    pub sidecar_directory: &'a Path,
    pub source_public_key: [u8; 32],
    pub secret: &'a str,
    pub cancel: &'a AtomicBool,
    pub progress: &'a dyn Fn(SealStage) -> io::Result<()>,
}

/// A fresh preparation secret. Hex keeps the canonical carrier's 15-character
/// policy satisfied while giving 256 bits of machine randomness; it is never
/// shown to the person and never used as a login password.
///
/// # Errors
///
/// The operating system random source fails.
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
