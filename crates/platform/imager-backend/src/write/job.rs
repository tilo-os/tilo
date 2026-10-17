//! Native write lifecycle: reserve, prepare, copy, customize, finalize, release.
#[cfg(test)]
mod tests;
#[cfg(target_os = "linux")]
use super::attest_target_descriptor;
use super::{
    JobResult, acquire_target_write_lock, finish_target,
    helper::{HelperPhase, HelperProgress, ReviewedSource, progress_fraction},
    prepare_target, revalidate_target, stream_copy_bound, verify_readback_bound,
};
use crate::media_model::SourceIdentity;
#[cfg(target_os = "linux")]
use std::fs::OpenOptions;
use std::{io, path::Path};

/// Native job geometry and immutable reviewed input, without app profile types.
pub struct SystemWriteRequest {
    pub source: ReviewedSource,
    pub target: crate::media_model::TargetDrive,
    pub expected_sha256: Option<String>,
    pub installable: bool,
    pub provision_migration_data: bool,
}

/// App-specific setup authorization and formats. Native device custody and
/// the lifecycle around these adapters stay in this module.
pub trait WriteSetup {
    /// Check authorization before any target preparation.
    ///
    /// # Errors
    /// Returns an error if setup policy or authenticated custody refuses the write.
    fn output_preflight(&self) -> io::Result<()>;
    /// Revalidate the migration sources independently of the target.
    ///
    /// # Errors
    /// Returns an error if a source changed, overlaps the target or cannot be inspected.
    fn validate_pending_source_disks(&self) -> io::Result<()>;
    fn write_descriptor(
        &self,
        device: &mut std::fs::File,
        should_cancel: &dyn Fn() -> bool,
        on_progress: &mut dyn FnMut(HelperProgress),
    ) -> JobResult;
    /// Encode, persist and verify the installation profile.
    ///
    /// # Errors
    /// Returns encoding, destination I/O or read-back validation errors, or
    /// `io::ErrorKind::Interrupted` when cancelled.
    fn write_profile(&self, destination: &Path, should_cancel: &dyn Fn() -> bool)
    -> io::Result<()>;
    /// Apply any staged setup handoff to the written destination.
    ///
    /// # Errors
    /// Returns an error if the destination cannot safely receive the handoff or
    /// installation or embedding fails.
    fn apply_handoff(&self, destination: &Path) -> io::Result<()>;
    /// Provision protected migration output after writing and handoff.
    ///
    /// # Errors
    /// Returns authorization, provisioning or validation errors, or
    /// `io::ErrorKind::Interrupted` when cancelled.
    fn provision_after_write(&self, should_cancel: &dyn Fn() -> bool) -> io::Result<()>;
    fn release_handoff(&self);
}

pub fn run_job_paths_bound(
    src: &Path,
    dst: &Path,
    total: u64,
    expected_identity: Option<SourceIdentity>,
    expected_sha256: Option<&str>,
    should_cancel: &impl Fn() -> bool,
    mut on_progress: impl FnMut(HelperProgress),
) -> JobResult {
    let total = total.max(1);
    // Write phase.
    let copied = stream_copy_bound(
        src,
        dst,
        total,
        expected_identity,
        expected_sha256,
        should_cancel,
        |bytes| {
            on_progress(HelperProgress {
                phase: HelperPhase::Writing,
                write_frac: progress_fraction(bytes, total),
                verify_frac: 0.0,
                rate_bps: 0,
                error: None,
            });
        },
    );
    let written = match copied {
        Ok(n) => n,
        Err(e) if e.kind() == io::ErrorKind::Interrupted => return JobResult::Cancelled,
        Err(e) => return JobResult::WriteFailed(e.to_string()),
    };
    // Verify phase over exactly the bytes we wrote.
    let verify = verify_readback_bound(src, dst, written, should_cancel, |bytes| {
        on_progress(HelperProgress {
            phase: HelperPhase::Verifying,
            write_frac: 1.0,
            verify_frac: progress_fraction(bytes, written.max(1)),
            rate_bps: 0,
            error: None,
        });
    });
    match verify {
        // This is not yet the user-visible terminal success: installable media
        // still needs its profile block written and read back, and the device
        // still needs platform finalisation. `run_system_job` publishes the
        // single terminal event only after all of those operations finish.
        Ok(None) => JobResult::Verified,
        Ok(Some(off)) => JobResult::VerifyMismatch(off),
        Err(e) if e.kind() == io::ErrorKind::Interrupted => JobResult::Cancelled,
        Err(e) => JobResult::VerifyFailed(e.to_string()),
    }
}

/// Execute through the descriptor supplied by the host authorization broker.
#[cfg(all(target_os = "linux", feature = "flatpak-media-writer"))]
pub fn run_udisks_job(
    target: &crate::media_model::TargetDrive,
    setup: &impl WriteSetup,
    should_cancel: &impl Fn() -> bool,
    mut on_progress: impl FnMut(HelperProgress),
) -> JobResult {
    let mut write_frac = 0.0;
    let mut verify_frac = 0.0;
    let mut relay = |progress: HelperProgress| {
        write_frac = progress.write_frac;
        verify_frac = progress.verify_frac;
        on_progress(progress);
    };
    if let Err(error) = setup.validate_pending_source_disks() {
        return JobResult::WriteFailed(error.to_string());
    }
    let mut device = match super::udisks::open_udisks_device(target) {
        Ok(device) => device,
        Err(error) => {
            setup.release_handoff();
            let result = JobResult::WriteFailed(error.to_string());
            on_progress(HelperProgress {
                phase: HelperPhase::Failed,
                error: Some(format!("Could not authorize this drive: {error}")),
                ..HelperProgress::default()
            });
            return result;
        }
    };
    let mut result = setup.write_descriptor(&mut device, should_cancel, &mut relay);
    drop(device);
    if result.is_success()
        && let Err(error) = setup.apply_handoff(Path::new(&target.dev_path()))
    {
        result = JobResult::VerifyFailed(format!(
            "could not apply Wi-Fi and identity onto the written disk: {error}"
        ));
    }
    setup.release_handoff();
    let error = match &result {
        JobResult::Verified => None,
        JobResult::WriteFailed(message) => Some(format!("Could not write the drive: {message}")),
        JobResult::VerifyMismatch(offset) => Some(format!(
            "Verification found different data at byte {offset}. The drive may be faulty."
        )),
        JobResult::VerifyFailed(message) => {
            Some(format!("Could not verify the finished drive: {message}"))
        }
        JobResult::Cancelled => Some(
            "Image writing was cancelled. The drive may be incomplete; review and retry before using it."
                .into(),
        ),
    };
    on_progress(HelperProgress {
        phase: if result.is_success() {
            HelperPhase::Success
        } else {
            HelperPhase::Failed
        },
        write_frac,
        verify_frac,
        rate_bps: 0,
        error,
    });
    result
}

/// Execute one complete native device transaction, retaining exclusion through finalization.
#[allow(
    clippy::too_many_lines,
    reason = "one linear lifecycle keeps target exclusion held through setup effects and platform finalization"
)]
pub fn run_system_job(
    request: &SystemWriteRequest,
    setup: &impl WriteSetup,
    should_cancel: &impl Fn() -> bool,
    mut on_progress: impl FnMut(HelperProgress),
) -> JobResult {
    let src = &request.source.path;
    let dst = std::path::PathBuf::from(request.target.dev_path());
    let mut prepared = false;
    let mut write_frac = 0.0;
    let mut verify_frac = 0.0;
    // Retain this guard until after finish_target. Separate elevated helpers
    // rendezvous here before either one unmounts, offlines, writes, verifies,
    // embeds a profile, or ejects the same physical target.
    let mut target_lock = None;
    let mut relay_progress = |progress: HelperProgress| {
        write_frac = progress.write_frac;
        verify_frac = progress.verify_frac;
        on_progress(progress);
    };
    let output_preflight = setup.output_preflight();
    let mut result = if output_preflight.is_err() {
        JobResult::WriteFailed("Original migration requires authenticated helper custody".into())
    } else if should_cancel() {
        JobResult::Cancelled
    } else if let Err(error) = revalidate_target(&request.target) {
        JobResult::WriteFailed(format!(
            "the selected drive changed or was disconnected: {error}"
        ))
    } else if let Err(error) = acquire_target_write_lock(&request.target).map(|lock| {
        target_lock = Some(lock);
    }) {
        JobResult::WriteFailed(format!("could not exclusively reserve the drive: {error}"))
    } else if let Err(error) = setup.validate_pending_source_disks() {
        JobResult::WriteFailed(error.to_string())
    } else if let Err(error) = prepare_target(&request.target) {
        JobResult::WriteFailed(format!(
            "the drive is still in use and could not be prepared: {error}"
        ))
    } else {
        prepared = true;
        #[cfg(target_os = "linux")]
        if request.provision_migration_data {
            use std::os::unix::fs::OpenOptionsExt;
            match OpenOptions::new()
                .read(true)
                .write(true)
                .custom_flags(libc::O_EXCL | libc::O_SYNC | libc::O_NOFOLLOW)
                .open(&dst)
            {
                Ok(mut device) => match attest_target_descriptor(&request.target, &device) {
                    Ok(()) => {
                        setup.write_descriptor(&mut device, should_cancel, &mut relay_progress)
                    }
                    Err(error) => JobResult::WriteFailed(format!(
                        "the opened drive no longer matches review: {error}"
                    )),
                },
                Err(error) => JobResult::WriteFailed(error.to_string()),
            }
        } else {
            run_job_paths_bound(
                src,
                &dst,
                request.source.size,
                request.source.identity,
                request.expected_sha256.as_deref(),
                should_cancel,
                &mut relay_progress,
            )
        }
        #[cfg(not(target_os = "linux"))]
        run_job_paths_bound(
            src,
            &dst,
            request.source.size,
            request.source.identity,
            request.expected_sha256.as_deref(),
            should_cancel,
            &mut relay_progress,
        )
    };
    if prepared {
        if result.is_success() && request.installable && !request.provision_migration_data {
            // The profile is written through a second open because the
            // pathname backend's streaming verifier owns and then closes its
            // descriptor.  Revalidate immediately before that open so a
            // disconnect/replug cannot make the profile land on a replacement
            // device which happens to reuse the same path.
            if let Err(error) = revalidate_target(&request.target) {
                result = JobResult::VerifyFailed(format!(
                    "could not revalidate the drive before embedding the setup profile: {error}"
                ));
            } else if let Err(error) = setup.write_profile(&dst, should_cancel) {
                result = if error.kind() == io::ErrorKind::Interrupted {
                    JobResult::Cancelled
                } else {
                    JobResult::VerifyFailed(format!(
                        "could not embed and verify the setup profile: {error}"
                    ))
                };
            }
        }
        if result.is_success()
            && let Err(error) = setup.apply_handoff(&dst)
        {
            result = JobResult::VerifyFailed(format!(
                "could not apply Wi-Fi and identity onto the written disk: {error}"
            ));
        }
        if result.is_success() && should_cancel() {
            result = JobResult::Cancelled;
        }
        if result.is_success()
            && let Err(error) = setup.provision_after_write(should_cancel)
        {
            result = if error.kind() == io::ErrorKind::Interrupted {
                JobResult::Cancelled
            } else {
                JobResult::VerifyFailed("Protected migration output provisioning refused".into())
            };
        }
        finish_target(&request.target, &result);
    }
    setup.release_handoff();
    // Keep the per-device exclusion through platform finalization, then make
    // its release point explicit before reporting the terminal event.
    drop(target_lock);
    let error = match &result {
        JobResult::Verified => None,
        JobResult::WriteFailed(message) => Some(format!("Could not write the drive: {message}")),
        JobResult::VerifyMismatch(offset) => Some(format!(
            "Verification found different data at byte {offset}. The drive may be faulty."
        )),
        JobResult::VerifyFailed(message) => {
            Some(format!("Could not verify the finished drive: {message}"))
        }
        JobResult::Cancelled => Some(
            "Image writing was cancelled. The drive may be incomplete; review and retry before using it."
                .into(),
        ),
    };
    on_progress(HelperProgress {
        phase: if result.is_success() {
            HelperPhase::Success
        } else {
            HelperPhase::Failed
        },
        write_frac,
        verify_frac,
        rate_bps: 0,
        error,
    });
    result
}
