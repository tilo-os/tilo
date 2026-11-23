//! One held-descriptor copy and profile transaction.
#[cfg(test)]
mod tests;
use super::helper::ReviewedSource;
#[cfg(any(test, feature = "test-support", target_os = "linux"))]
use super::{
    JobResult,
    helper::{HelperPhase, HelperProgress, progress_fraction},
    read_exact_fill,
};
#[cfg(any(test, feature = "test-support", target_os = "linux"))]
use crate::media_model::PROFILE_BLOCK_SIZE;
#[cfg(any(test, feature = "test-support", target_os = "linux"))]
use std::io::{self, Seek, SeekFrom, Write};

/// Neutral inputs held for the full authorized-descriptor transaction.
pub struct DescriptorWriteRequest {
    pub source: ReviewedSource,
    pub expected_sha256: Option<String>,
    pub target_size: u64,
    pub installable: bool,
}

/// The app supplies only profile encoding. All raw I/O, digest validation,
/// synchronization and read-back live here.
#[cfg(any(test, feature = "test-support", target_os = "linux"))]
pub fn run_job_file_bound(
    request: &DescriptorWriteRequest,
    device: &mut std::fs::File,
    should_cancel: &impl Fn() -> bool,
    mut on_progress: impl FnMut(HelperProgress),
    encode_profile: impl FnOnce() -> io::Result<Vec<u8>>,
) -> JobResult {
    let mut source = match super::open_reviewed_source(
        &request.source.path,
        request.source.size,
        request.source.identity,
        request.expected_sha256.as_deref(),
        should_cancel,
    ) {
        Ok(source) => source,
        Err(result) => return result,
    };
    let result = super::copy_verify_descriptor(
        &mut source,
        device,
        request.source.size,
        request.expected_sha256.as_deref(),
        should_cancel,
        |progress| {
            use super::CopyProgress;
            let (phase, write_frac, verify_frac) = match progress {
                CopyProgress::Writing { bytes, total } => {
                    (HelperPhase::Writing, progress_fraction(bytes, total), 0.0)
                }
                CopyProgress::Verifying { bytes, total } => {
                    (HelperPhase::Verifying, 1.0, progress_fraction(bytes, total))
                }
            };
            on_progress(HelperProgress {
                phase,
                write_frac,
                verify_frac,
                rate_bps: 0,
                error: None,
            });
        },
    );
    if !result.is_success() {
        return result;
    }
    if request.installable {
        let block = match encode_profile() {
            Ok(block) => block,
            Err(error) => return JobResult::VerifyFailed(error.to_string()),
        };
        if block.len() != PROFILE_BLOCK_SIZE {
            return JobResult::VerifyFailed(
                "installation profile has an invalid block length".into(),
            );
        }
        let Some(offset) = request.target_size.checked_sub(PROFILE_BLOCK_SIZE as u64) else {
            return JobResult::VerifyFailed("target is too small for installation profile".into());
        };
        if let Err(error) = device
            .seek(SeekFrom::Start(offset))
            .and_then(|_| device.write_all(&block))
            .and_then(|()| device.flush())
            .and_then(|()| device.sync_all())
            .and_then(|()| device.seek(SeekFrom::Start(offset)).map(|_| ()))
        {
            return JobResult::VerifyFailed(error.to_string());
        }
        let mut actual = vec![0_u8; PROFILE_BLOCK_SIZE];
        if let Err(error) = read_exact_fill(device, &mut actual) {
            return JobResult::VerifyFailed(error.to_string());
        }
        if actual != block {
            return JobResult::VerifyFailed("installation profile read-back did not match".into());
        }
    }
    JobResult::Verified
}
