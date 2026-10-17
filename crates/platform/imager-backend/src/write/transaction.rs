//! One held-descriptor copy, migration geometry commit, and profile transaction.
#[cfg(test)]
mod tests;
use super::helper::ReviewedSource;
#[cfg(any(test, target_os = "linux"))]
use super::{
    JobResult,
    helper::{HelperPhase, HelperProgress, progress_fraction},
    read_exact_fill,
};
#[cfg(any(test, target_os = "linux"))]
use crate::media_model::PROFILE_BLOCK_SIZE;
#[cfg(any(test, target_os = "linux"))]
use std::io::{self, Seek, SeekFrom, Write};

#[cfg(target_os = "linux")]
pub struct ExpectedLayout {
    pub binding: crate::media_layout::MediaPartitionBinding,
    pub reserved_bytes: u64,
}

/// Neutral inputs held for the full authorized-descriptor transaction.
pub struct DescriptorWriteRequest {
    pub source: ReviewedSource,
    pub expected_sha256: Option<String>,
    pub target_size: u64,
    pub target_stable_id: String,
    pub installable: bool,
    pub provision_migration_data: bool,
    pub has_legacy_handoff: bool,
    #[cfg(target_os = "linux")]
    pub expected_layout: Option<ExpectedLayout>,
}

/// The app supplies only profile encoding and migration policy. All raw I/O,
/// digest validation, GPT commitment, synchronization and read-back live here.
#[cfg(any(test, target_os = "linux"))]
#[allow(
    clippy::too_many_lines,
    reason = "the held descriptor remains borrowed through one linear copy, layout, and profile transaction"
)]
pub fn run_job_file_bound(
    request: &DescriptorWriteRequest,
    device: &mut std::fs::File,
    should_cancel: &impl Fn() -> bool,
    mut on_progress: impl FnMut(HelperProgress),
    validate_pending: impl Fn(&std::fs::File) -> io::Result<()>,
    payload_capacity: fn(u64) -> io::Result<u64>,
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
    #[cfg(target_os = "linux")]
    let layout = if request.provision_migration_data {
        if let Err(error) = validate_pending(device) {
            return JobResult::WriteFailed(error.to_string());
        }
        if !request.installable || request.has_legacy_handoff {
            return JobResult::WriteFailed("Migration layout refuses legacy tail handoff".into());
        }
        let prepared = if let Some(pending) = &request.expected_layout {
            crate::media_layout::prepare_expected(
                &source,
                device,
                request.source.size,
                request.target_size,
                &pending.binding,
                pending.reserved_bytes,
                payload_capacity,
            )
        } else {
            crate::media_layout::prepare(
                &source,
                device,
                request.source.size,
                request.target_size,
                &request.target_stable_id,
                payload_capacity,
            )
        };
        match prepared {
            Ok(layout) => Some(layout),
            Err(error) => return JobResult::WriteFailed(error.to_string()),
        }
    } else {
        None
    };
    #[cfg(not(target_os = "linux"))]
    if request.provision_migration_data {
        return JobResult::WriteFailed("Migration data provisioning requires Linux".into());
    }
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
    #[cfg(target_os = "linux")]
    let profile_offset = match layout {
        Some(layout) => match layout.commit(device, should_cancel) {
            Ok(binding) => {
                if request
                    .expected_layout
                    .as_ref()
                    .is_some_and(|pending| pending.binding != binding)
                {
                    return JobResult::VerifyFailed(
                        "Committed migration layout differs from sealed expectation".into(),
                    );
                }
                Some(binding.profile_offset)
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                return JobResult::Cancelled;
            }
            Err(error) => return JobResult::VerifyFailed(error.to_string()),
        },
        None => None,
    };
    #[cfg(not(target_os = "linux"))]
    let profile_offset: Option<u64> = None;
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
        let Some(offset) =
            profile_offset.or_else(|| request.target_size.checked_sub(PROFILE_BLOCK_SIZE as u64))
        else {
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
