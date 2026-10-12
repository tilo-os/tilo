//! Protocol6 selected applications-only export. The root-authenticated claim and
//! fixed source projection supply permission; no Home inventory is constructed.
use super::*;
use crate::setup::{
    flatpak_transfer::portable::{NativeOfflineRunner, StageGrant, StorageRequirement},
    portable_transfer::Observer,
};
use std::{os::fd::AsRawFd, os::unix::fs::DirBuilderExt};
const RECEIPT_RESERVE: u64 = 128 * 1024;
fn checked_sum(values: &[u64]) -> io::Result<u64> {
    values.iter().try_fold(0u64, |sum, value| {
        sum.checked_add(*value)
            .ok_or_else(|| failure("Applications-only storage bound overflows"))
    })
}
fn available(required: u64, reservation: u64) -> io::Result<()> {
    if required > reservation {
        return Err(failure(
            "Selected applications exceed admitted medium budget",
        ));
    }
    let output =
        PinnedDirectory::open_absolute(Path::new(launcher::owned_service::CAPTURE_OUTPUT_PATH))?;
    output.validate_destination()?;
    let mut stats: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstatvfs(output.file.as_raw_fd(), &raw mut stats) } < 0 {
        return Err(io::Error::last_os_error());
    }
    let free = stats
        .f_bavail
        .checked_mul(stats.f_frsize)
        .ok_or_else(|| failure("Applications-only free space overflows"))?;
    if stats.f_flag & libc::ST_RDONLY != 0 || required > free {
        return Err(failure(
            "Selected applications output and scratch space unavailable",
        ));
    }
    Ok(())
}
fn export_bound(requirement: &StorageRequirement) -> io::Result<u64> {
    checked_sum(&[
        requirement.staging_bytes,
        requirement.staging_bytes,
        requirement.sealed_bytes,
        RECEIPT_RESERVE,
    ])
}
pub(super) fn execute(
    command: Command,
    pin: std::sync::Arc<PinnedDirectory>,
    cancel: &AtomicBool,
    observer: &mut Observer<'_>,
) -> io::Result<Response> {
    let authority = command
        .original_authority()
        .cloned()
        .ok_or_else(|| failure("Applications-only command lacks original authority"))?;
    authority.validate_applications_only(None)?;
    // This authenticates root-owned sealed FD50 and original credentials before
    // accessing either the closed installation or any selected app-data path.
    let _mounted = authority.validate_reader(&pin.file)?;
    let claim = authority
        .flatpak()
        .ok_or_else(|| failure("Selected application claim missing"))?;
    let source = claim.review_applications_reader(authority.export_operation(), cancel)?;
    let requirement = source.storage_requirement(true)?;
    let response = match command {
        Command::ReviewOriginal { .. } => Response::ReviewOriginalApplications {
            staging_bytes: requirement.staging_bytes,
            sealed_bytes: export_bound(&requirement)?,
        },
        Command::ExportOriginalApplications { password, .. } => {
            available(export_bound(&requirement)?, authority.sealed_reservation())?;
            let password = password.take();
            let output = Path::new(launcher::owned_service::CAPTURE_OUTPUT_PATH);
            let scratch = tempfile::Builder::new()
                .prefix(".tilo-original-applications-")
                .tempdir_in(output)?;
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
                    include_user_state: true,
                },
                cancel,
            )?;
            let operation = authority.applications_operation()?;
            let applications = stage.seal_reporting(
                &output.join(format!("applications-{operation}.sealed")),
                operation,
                authority.setup_binding(),
                password,
                requirement.limits,
                cancel,
                observer,
            )?;
            authority.validate_applications_only(Some(&applications))?;
            stage.validate()?;
            drop(stage);
            // No finished response is emitted until owned plaintext disappears.
            scratch.close()?;
            Response::ExportOriginalApplications { applications }
        }
        Command::ValidateOriginalApplications {
            expected, password, ..
        } => {
            authority.validate_applications_only(Some(&expected))?;
            let occupied = checked_sum(&[
                expected.sealed_bytes,
                expected.plaintext_bytes,
                requirement.staging_bytes,
                RECEIPT_RESERVE,
            ])?;
            let needed = checked_sum(&[
                expected.plaintext_bytes,
                requirement.staging_bytes,
                RECEIPT_RESERVE,
            ])?;
            if occupied > authority.sealed_reservation() {
                return Err(failure(
                    "Applications-only verification exceeds admitted budget",
                ));
            }
            available(needed, authority.sealed_reservation())?;
            let output = Path::new(launcher::owned_service::CAPTURE_OUTPUT_PATH);
            let scratch = tempfile::Builder::new()
                .prefix(".tilo-original-applications-verify-")
                .tempdir_in(output)?;
            let quarantine = scratch.path().join("quarantine");
            std::fs::DirBuilder::new().mode(0o700).create(&quarantine)?;
            source.verify_sealed(
                &expected,
                &output.join(&expected.filename),
                &password.take(),
                &quarantine,
                cancel,
            )?;
            scratch.close()?;
            Response::Validated
        }
        _ => {
            return Err(failure(
                "Full-Home command cannot use applications-only authority",
            ));
        }
    };
    source.validate()?;
    authority.validate_reader(&pin.file)?;
    pin.validate()?;
    crate::setup::migration::check_cancel(cancel)?;
    Ok(response)
}
