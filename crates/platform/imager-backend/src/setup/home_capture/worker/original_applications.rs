//! Original Home plus selected system applications share one approved medium
//! reservation. Plaintext subset staging stays on that same output filesystem.
use super::*;
use crate::setup::{
    flatpak_transfer::portable::{
        NativeOfflineRunner, PortableFlatpakSource, StageGrant, StorageRequirement,
    },
    portable_transfer::Observer,
};
use std::{os::fd::AsRawFd, os::unix::fs::DirBuilderExt, sync::Arc};
const RECEIPT_RESERVE: u64 = 128 * 1024;
fn selected(
    authority: &OriginalReadClaim,
    cancel: &AtomicBool,
) -> io::Result<Option<Arc<PortableFlatpakSource>>> {
    authority
        .flatpak()
        .map(|claim| claim.review_reader(authority.export_operation(), cancel))
        .transpose()
}
fn checked_total(values: &[u64]) -> io::Result<u64> {
    values.iter().try_fold(0u64, |sum, value| {
        sum.checked_add(*value)
            .ok_or_else(|| failure("Original aggregate storage bound overflows"))
    })
}
fn available(required: u64) -> io::Result<()> {
    let pin =
        PinnedDirectory::open_absolute(Path::new(launcher::owned_service::CAPTURE_OUTPUT_PATH))?;
    pin.validate_destination()?;
    let mut stats: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: held output descriptor and writable statvfs result.
    if unsafe { libc::fstatvfs(pin.file.as_raw_fd(), &raw mut stats) } < 0 {
        return Err(io::Error::last_os_error());
    }
    let free = stats
        .f_bavail
        .checked_mul(stats.f_frsize)
        .ok_or_else(|| failure("Original output space overflows"))?;
    if stats.f_flag & libc::ST_RDONLY != 0 || required > free {
        return Err(failure(
            "Original combined output and scratch space unavailable",
        ));
    }
    Ok(())
}
fn preflight(
    home: u64,
    applications: Option<&StorageRequirement>,
    reservation: u64,
) -> io::Result<()> {
    let required = checked_total(&[
        home,
        applications.map_or(0, |item| item.staging_bytes),
        applications.map_or(0, |item| item.staging_bytes),
        applications.map_or(0, |item| item.sealed_bytes),
        if applications.is_some() {
            RECEIPT_RESERVE
        } else {
            0
        },
    ])?;
    if required > reservation {
        return Err(failure(
            "Original selected data exceeds aggregate medium reservation",
        ));
    }
    available(required)
}
/// Phase-local archive byte counts from separate canonical exporters are
/// accumulated, preserving the existing authenticated monotonic progress wire.
struct Totals([u64; 3]);
impl Totals {
    fn index(phase: ArchivePhase) -> io::Result<usize> {
        match phase {
            ArchivePhase::Archiving => Ok(0),
            ArchivePhase::VerifyingArchive => Ok(1),
            ArchivePhase::VerifyingSource => Ok(2),
            ArchivePhase::DeliveringCiphertext => Err(failure("Unexpected subset delivery phase")),
        }
    }
    fn observe(&mut self, progress: ArchiveProgress) -> io::Result<()> {
        if progress.phase != ArchivePhase::DeliveringCiphertext {
            self.0[Self::index(progress.phase)?] = progress.completed;
        }
        Ok(())
    }
    fn offset(&self, mut progress: ArchiveProgress) -> io::Result<ArchiveProgress> {
        let index = Self::index(progress.phase)?;
        progress.completed = progress
            .completed
            .checked_add(self.0[index])
            .ok_or_else(|| failure("Combined archive progress overflows"))?;
        Ok(progress)
    }
}
pub(super) fn export(
    home: &HomeSource,
    authority: &OriginalReadClaim,
    operation: &str,
    binding: &str,
    password: Zeroizing<Vec<u8>>,
    cancel: &AtomicBool,
    observer: &mut Observer<'_>,
) -> io::Result<Response> {
    authority.validate_export(operation, binding, None)?;
    let applications = selected(authority, cancel)?;
    let requirement = applications
        .as_ref()
        .map(|source| source.storage_requirement(false))
        .transpose()?;
    let home_bound = home.sealed_reservation()?;
    preflight(
        home_bound,
        requirement.as_ref(),
        authority.sealed_reservation(),
    )?;
    let output_path = Path::new(launcher::owned_service::CAPTURE_OUTPUT_PATH);
    let output = MediaGrant::open(output_path)?.limit_original_reservation(home_bound)?;
    let mut totals = Totals([0; 3]);
    let expected = home.export_to_reporting(
        &output,
        operation,
        binding,
        Zeroizing::new(password.to_vec()),
        cancel,
        &mut |progress| {
            totals.observe(progress)?;
            observer(progress)
        },
    )?;
    let applications = match (applications, requirement) {
        (Some(source), Some(requirement)) => {
            crate::setup::migration::check_cancel(cancel)?;
            let scratch = tempfile::Builder::new()
                .prefix(".tilo-original-applications-")
                .tempdir_in(output_path)?;
            let tree = scratch.path().join("tree");
            let journals = scratch.path().join("journals");
            std::fs::DirBuilder::new().mode(0o700).create(&tree)?;
            std::fs::DirBuilder::new().mode(0o700).create(&journals)?;
            let stage = source.stage(
                &NativeOfflineRunner,
                StageGrant {
                    root: &tree,
                    journals: &journals,
                    allocation_bytes: requirement.staging_bytes,
                    include_user_state: false,
                },
                cancel,
            )?;
            let app_operation = authority.applications_operation()?;
            let package = output_path.join(format!("applications-{app_operation}.sealed"));
            let expected = stage.seal_reporting(
                &package,
                app_operation,
                binding,
                Zeroizing::new(password.to_vec()),
                requirement.limits,
                cancel,
                &mut |progress| observer(totals.offset(progress)?),
            )?;
            stage.validate()?;
            drop(stage);
            // Successful completion requires observed scratch cleanup, not Drop's
            // best-effort removal. No plaintext staging is published in a receipt.
            scratch.close()?;
            Some(expected)
        }
        (None, None) => None,
        _ => return Err(failure("Original application source bound disappeared")),
    };
    authority.validate_export(operation, binding, Some(&expected))?;
    authority.validate_applications(applications.as_ref(), &expected)?;
    home.validate_with_cancel(cancel)?;
    Ok(Response::ExportOriginal {
        home: expected,
        applications,
    })
}
pub(super) fn validate(
    home: &HomeSource,
    authority: &OriginalReadClaim,
    expected: (
        &Expectation,
        Option<&crate::setup::flatpak_transfer::portable::Expectation>,
    ),
    binding: &str,
    password: &[u8],
    cancel: &AtomicBool,
    observer: &mut Observer<'_>,
) -> io::Result<()> {
    authority.validate_export(authority.export_operation(), binding, Some(expected.0))?;
    authority.validate_applications(expected.1, expected.0)?;
    let applications = selected(authority, cancel)?;
    let output_path = Path::new(launcher::owned_service::CAPTURE_OUTPUT_PATH);
    let output = MediaGrant::open(output_path)?
        .limit_original_reservation(authority.sealed_reservation())?;
    // Home's canonical verifier streams and creates no plaintext duplicate.
    home.validate_existing_reporting(&output, expected.0, binding, password, cancel, observer)?;
    let home_bytes = expected.0.sealed_bytes;
    match (applications, expected.1) {
        (Some(source), Some(expected)) => {
            let requirement = source.storage_requirement(false)?;
            let required = checked_total(&[
                expected.plaintext_bytes,
                requirement.staging_bytes,
                RECEIPT_RESERVE,
            ])?;
            let occupied = checked_total(&[home_bytes, expected.sealed_bytes, required])?;
            if occupied > authority.sealed_reservation() {
                return Err(failure(
                    "Original verification scratch exceeds aggregate reservation",
                ));
            }
            available(required)?;
            let scratch = tempfile::Builder::new()
                .prefix(".tilo-original-verify-")
                .tempdir_in(output_path)?;
            let quarantine = scratch.path().join("quarantine");
            std::fs::DirBuilder::new().mode(0o700).create(&quarantine)?;
            source.verify_sealed(
                expected,
                &output_path.join(&expected.filename),
                password,
                &quarantine,
                cancel,
            )?;
            scratch.close()?;
        }
        (None, None) => {}
        _ => {
            return Err(failure(
                "Original selected applications verification missing",
            ));
        }
    }
    home.validate_with_cancel(cancel)
}
