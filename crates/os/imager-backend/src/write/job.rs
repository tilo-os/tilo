//! Native write lifecycle: reserve, prepare, copy, customize, finalize, release.
#[cfg(test)]
mod tests;
use super::{
    JobResult, acquire_target_write_lock, finish_target,
    helper::{HelperPhase, HelperProgress, ReviewedSource, progress_fraction},
    prepare_target, revalidate_target, stream_copy_bound, verify_readback_bound,
};
use crate::imager_model::SourceIdentity;
use std::{io, path::Path};

pub struct SystemWriteRequest {
    /// The source as the choice opened it, or why it could not be: a drive that cannot be written
    /// is reported first, before the source is read.
    pub source: Result<ReviewedSource, String>,
    pub target: crate::imager_model::TargetDrive,
    pub expected_sha256: Option<String>,
    pub installable: bool,
}

/// Device identity and the lifecycle around these adapters stay in this module.
pub trait WriteSetup {
    fn write_descriptor(
        &self,
        device: &mut std::fs::File,
        should_cancel: &dyn Fn() -> bool,
        on_progress: &mut dyn FnMut(HelperProgress),
    ) -> JobResult;
    /// Returns encoding, destination I/O or read-back validation errors, or
    /// `io::ErrorKind::Interrupted` when cancelled.
    fn write_profile(&self, destination: &Path, should_cancel: &dyn Fn() -> bool)
    -> io::Result<()>;
    fn apply_handoff(&self, destination: &Path) -> io::Result<()>;
    fn release_handoff(&self);
}

pub fn run_job_paths_bound(
    src: &Path,
    dst: &Path,
    total: u64,
    expected_identity: Option<SourceIdentity>,
    expected_sha256: Option<&str>,
    should_cancel: &impl Fn() -> bool,
    on_progress: impl FnMut(HelperProgress),
) -> JobResult {
    let mut source = match std::fs::OpenOptions::new().read(true).open(src) {
        Ok(source) => source,
        Err(error) => return JobResult::WriteFailed(error.to_string()),
    };
    run_job_file_bound(
        &mut source,
        dst,
        total,
        expected_identity,
        expected_sha256,
        should_cancel,
        on_progress,
    )
}

/// [`run_job_paths_bound`] for the source as the person's choice opened it.
pub fn run_job_source_bound(
    source: &ReviewedSource,
    dst: &Path,
    expected_sha256: Option<&str>,
    should_cancel: &impl Fn() -> bool,
    on_progress: impl FnMut(HelperProgress),
) -> JobResult {
    let mut file = match source.reader() {
        Ok(file) => file,
        Err(error) => return JobResult::WriteFailed(error.to_string()),
    };
    run_job_file_bound(
        &mut file,
        dst,
        source.size,
        source.identity,
        expected_sha256,
        should_cancel,
        on_progress,
    )
}

fn run_job_file_bound(
    src: &mut std::fs::File,
    dst: &Path,
    total: u64,
    expected_identity: Option<SourceIdentity>,
    expected_sha256: Option<&str>,
    should_cancel: &impl Fn() -> bool,
    mut on_progress: impl FnMut(HelperProgress),
) -> JobResult {
    let total = total.max(1);
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
        // The single terminal success comes later, after the profile is written and read back and
        // the device is finalised; `run_system_job` publishes it.
        Ok(None) => JobResult::Verified,
        Ok(Some(off)) => JobResult::VerifyMismatch(off),
        Err(e) if e.kind() == io::ErrorKind::Interrupted => JobResult::Cancelled,
        Err(e) => JobResult::VerifyFailed(e.to_string()),
    }
}

#[cfg(all(target_os = "linux", feature = "flatpak-media-writer"))]
pub fn run_udisks_job(
    target: &crate::imager_model::TargetDrive,
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
    let dst = std::path::PathBuf::from(request.target.dev_path());
    let mut prepared = false;
    let mut write_frac = 0.0;
    let mut verify_frac = 0.0;
    // Keep this guard until after finish_target: separate elevated helpers rendezvous here before
    // either one unmounts, offlines, writes, verifies, embeds a profile, or ejects the same target.
    let mut target_lock = None;
    let mut relay_progress = |progress: HelperProgress| {
        write_frac = progress.write_frac;
        verify_frac = progress.verify_frac;
        on_progress(progress);
    };
    let mut result = if should_cancel() {
        JobResult::Cancelled
    } else if let Err(error) = revalidate_target(&request.target) {
        JobResult::WriteFailed(format!(
            "the selected drive changed or was disconnected: {error}"
        ))
    } else if let Err(error) = acquire_target_write_lock(&request.target).map(|lock| {
        target_lock = Some(lock);
    }) {
        JobResult::WriteFailed(format!("could not exclusively reserve the drive: {error}"))
    } else if let Err(error) = prepare_target(&request.target) {
        JobResult::WriteFailed(format!(
            "the drive is still in use and could not be prepared: {error}"
        ))
    } else {
        prepared = true;
        match &request.source {
            Ok(source) => run_job_source_bound(
                source,
                &dst,
                request.expected_sha256.as_deref(),
                should_cancel,
                &mut relay_progress,
            ),
            Err(why) => JobResult::WriteFailed(why.clone()),
        }
    };
    if prepared {
        if result.is_success() && request.installable {
            // The profile is written through a second open because the streaming verifier owns and
            // closes its descriptor. Revalidate immediately before that open, so a replug cannot put
            // the profile on a replacement device that reuses the path.
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
        finish_target(&request.target, &result);
    }
    setup.release_handoff();
    // Keep the per-device exclusion through platform finalization, then release it explicitly
    // before the terminal event.
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
