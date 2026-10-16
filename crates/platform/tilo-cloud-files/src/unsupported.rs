//! Fail closed where directory identities and the cloud socket are unavailable.
use crate::{
    DestinationIdentity, ImportConsent, MoveInto, TransferOutcome, TransferProgress,
    TransferReceipt,
};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

const UNAVAILABLE: &str = "safe cloud file imports require a Unix host";

/// # Errors
/// Always returns an unsupported-platform error.
pub fn pin_destination<'a>(
    _: &str,
    _: impl Iterator<Item = &'a Path>,
) -> Result<DestinationIdentity, String> {
    Err(UNAVAILABLE.into())
}

/// # Errors
/// Always returns an unsupported-platform error.
pub fn compensate(
    _: &str,
    _: &DestinationIdentity,
    _: &DestinationIdentity,
    _: MoveInto,
) -> Result<TransferReceipt, String> {
    Err(UNAVAILABLE.into())
}

/// # Errors
/// Always returns an unsupported-platform error.
pub fn restore(
    _: &str,
    _: &str,
    _: &DestinationIdentity,
    _: &DestinationIdentity,
    _: MoveInto,
) -> Result<TransferReceipt, String> {
    Err(UNAVAILABLE.into())
}

/// Returns an explicit failure without touching paths or invoking callbacks.
pub fn execute(
    _: ImportConsent,
    _: &AtomicBool,
    _: impl Fn(TransferProgress),
    _: &[PathBuf],
) -> TransferOutcome {
    TransferOutcome::Failed {
        reason: UNAVAILABLE.into(),
        partial: Vec::new(),
    }
}

/// Returns an explicit failure without opening the supplied socket path.
pub fn execute_at(
    _: &Path,
    consent: ImportConsent,
    cancel: &AtomicBool,
    progress: impl Fn(TransferProgress),
    mounts: &[PathBuf],
) -> TransferOutcome {
    execute(consent, cancel, progress, mounts)
}
