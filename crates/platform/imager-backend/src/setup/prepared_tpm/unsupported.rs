//! Refusals for hosts without a TPM-backed boot policy.
//!
//! A medium prepared here can still defer sensitive work to the destination's
//! first boot, so resolving a target policy is honestly empty. Sealing or
//! verifying a sidecar is requested work this host cannot do, and it fails
//! rather than pretending to succeed.

use std::io;
use std::path::Path;

use super::{AcquireRequest, SealPreparationRequest, SealedPreparation, TargetAcquisition};

fn unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "Preparing a TPM-protected handoff requires the Linux installer",
    )
}

/// This host has no signed target boot policy to acquire.
///
/// # Errors
///
/// Never; the empty answer is the honest one on this host.
pub fn target_for_preparation(
    _request: &AcquireRequest<'_>,
) -> io::Result<Option<TargetAcquisition>> {
    Ok(None)
}

/// Sealing needs this machine's TPM.
///
/// # Errors
///
/// Always, with an unsupported-host refusal.
pub fn seal_prepared_handoff(
    _request: &SealPreparationRequest<'_>,
) -> io::Result<SealedPreparation> {
    Err(unsupported())
}

/// Verifying a sidecar needs the canonical prepared package custody this host
/// does not provide.
///
/// # Errors
///
/// Always, with an unsupported-host refusal.
pub fn preflight_handoff(_sidecar: &Path, _package: &Path) -> io::Result<()> {
    Err(unsupported())
}
