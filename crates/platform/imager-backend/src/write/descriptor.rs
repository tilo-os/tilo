//! Reviewed held-source and authorized-descriptor byte pipeline.
use super::{CHUNK, JobResult, first_diff, hash_source_exact, read_exact_fill};
use crate::media_model::SourceIdentity;
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    path::Path,
};

/// Open and validate the reviewed image, retaining its descriptor for writing.
///
/// # Errors
/// Returns `JobResult::Cancelled` if digest validation is cancelled, or
/// `JobResult::WriteFailed` on I/O errors or a changed file, size, identity or digest.
pub fn open_reviewed_source(
    path: &Path,
    size: u64,
    identity: Option<SourceIdentity>,
    expected_sha256: Option<&str>,
    should_cancel: &impl Fn() -> bool,
) -> Result<File, JobResult> {
    let mut source = match OpenOptions::new().read(true).open(path) {
        Ok(file) => file,
        Err(error) => return Err(JobResult::WriteFailed(error.to_string())),
    };
    let metadata = match source.metadata() {
        Ok(metadata) => metadata,
        Err(error) => return Err(JobResult::WriteFailed(error.to_string())),
    };
    if !metadata.is_file()
        || metadata.len() != size
        || identity.is_some_and(|expected| {
            SourceIdentity::from_file(&source).ok().flatten() != Some(expected)
        })
    {
        return Err(JobResult::WriteFailed(
            "source image changed after review".into(),
        ));
    }
    if let Some(expected) = identity {
        match SourceIdentity::from_file(&source) {
            Ok(Some(actual)) if actual == expected => {}
            Ok(_) => {
                return Err(JobResult::WriteFailed(
                    "source image changed after review".into(),
                ));
            }
            Err(error) => return Err(JobResult::WriteFailed(error.to_string())),
        }
    }
    if let Some(expected) = expected_sha256 {
        let actual = match hash_source_exact(&mut source, size, should_cancel) {
            Ok(digest) => digest,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                return Err(JobResult::Cancelled);
            }
            Err(error) => return Err(JobResult::WriteFailed(error.to_string())),
        };
        if !actual.eq_ignore_ascii_case(expected) {
            return Err(JobResult::WriteFailed(
                "source image no longer matches its signed release catalog".into(),
            ));
        }
    }

    Ok(source)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reviewed_source_and_held_descriptor_use_the_same_exact_bytes() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("source.img");
        let bytes = b"reviewed image bytes";
        std::fs::write(&path, bytes).unwrap();
        let held = File::open(&path).unwrap();
        let identity = SourceIdentity::from_file(&held).unwrap();
        let digest = hex::encode(Sha256::digest(bytes));
        let mut source =
            open_reviewed_source(&path, bytes.len() as u64, identity, Some(&digest), &|| {
                false
            })
            .unwrap();
        let mut destination = tempfile::tempfile().unwrap();
        let mut writes = 0;
        let mut verifies = 0;
        assert_eq!(
            copy_verify_descriptor(
                &mut source,
                &mut destination,
                bytes.len() as u64,
                Some(&digest),
                &|| false,
                |progress| match progress {
                    CopyProgress::Writing { bytes, .. } => writes = bytes,
                    CopyProgress::Verifying { bytes, .. } => verifies = bytes,
                },
            ),
            JobResult::Verified
        );
        assert_eq!(writes, bytes.len() as u64);
        assert_eq!(verifies, bytes.len() as u64);
        destination.seek(SeekFrom::Start(0)).unwrap();
        let mut actual = Vec::new();
        destination.read_to_end(&mut actual).unwrap();
        assert_eq!(actual, bytes);

        // A same-length mutation cannot pass the digest-bound source review.
        std::fs::write(&path, vec![b'x'; bytes.len()]).unwrap();
        assert!(matches!(
            open_reviewed_source(&path, bytes.len() as u64, identity, Some(&digest), &|| {
                false
            },),
            Err(JobResult::WriteFailed(_))
        ));
    }

    #[test]
    fn cancelled_descriptor_copy_never_reports_verified() {
        let mut source = tempfile::tempfile().unwrap();
        source.write_all(b"image").unwrap();
        let mut destination = tempfile::tempfile().unwrap();
        assert_eq!(
            copy_verify_descriptor(&mut source, &mut destination, 5, None, &|| true, |_| {},),
            JobResult::Cancelled
        );
        assert_eq!(destination.metadata().unwrap().len(), 0);
    }
}

/// Native byte counters; frontends choose presentation and rate estimation.
pub enum CopyProgress {
    Writing { bytes: u64, total: u64 },
    Verifying { bytes: u64, total: u64 },
}

pub fn copy_verify_descriptor(
    source: &mut File,
    device: &mut File,
    size: u64,
    expected_sha256: Option<&str>,
    should_cancel: &impl Fn() -> bool,
    mut on_progress: impl FnMut(CopyProgress),
) -> JobResult {
    if let Err(error) = source.seek(SeekFrom::Start(0)) {
        return JobResult::WriteFailed(error.to_string());
    }
    if let Err(error) = device.seek(SeekFrom::Start(0)) {
        return JobResult::WriteFailed(error.to_string());
    }

    let total = size.max(1);
    let mut buffer = vec![0_u8; CHUNK];
    let mut written = 0_u64;
    let mut digest = Sha256::new();
    while written < size {
        if should_cancel() {
            return JobResult::Cancelled;
        }
        let want = usize::try_from(size.saturating_sub(written).min(CHUNK as u64)).unwrap_or(CHUNK);
        let Some(chunk) = buffer.get_mut(..want) else {
            return JobResult::WriteFailed("source chunk exceeds buffer".into());
        };
        if let Err(error) = read_exact_fill(source, chunk) {
            return JobResult::WriteFailed(error.to_string());
        }
        if let Err(error) = device.write_all(chunk) {
            return JobResult::WriteFailed(error.to_string());
        }
        digest.update(&*chunk);
        written = written.saturating_add(want as u64);
        on_progress(CopyProgress::Writing {
            bytes: written,
            total,
        });
    }
    if expected_sha256
        .is_some_and(|expected| !hex::encode(digest.finalize()).eq_ignore_ascii_case(expected))
    {
        return JobResult::WriteFailed(
            "source image no longer matches its signed release catalog".into(),
        );
    }
    // The descriptor path has already checked the reviewed metadata, but a
    // local source can still grow while the copy is in flight.  Keep this
    // path as strict as the pathname writer: accepting only the reviewed
    // prefix would make the two production backends disagree about what was
    // actually flashed.
    let mut extra = [0_u8; 1];
    match source.read(&mut extra) {
        Ok(0) => {}
        Ok(_) => return JobResult::WriteFailed("source image became larger after review".into()),
        Err(error) => return JobResult::WriteFailed(error.to_string()),
    }
    if let Err(error) = device.flush().and_then(|()| device.sync_all()) {
        return JobResult::WriteFailed(error.to_string());
    }
    if let Err(error) = source.seek(SeekFrom::Start(0)) {
        return JobResult::VerifyFailed(error.to_string());
    }
    if let Err(error) = device.seek(SeekFrom::Start(0)) {
        return JobResult::VerifyFailed(error.to_string());
    }
    let mut source_buffer = vec![0_u8; CHUNK];
    let mut target_buffer = vec![0_u8; CHUNK];
    let mut verified = 0_u64;
    while verified < size {
        if should_cancel() {
            return JobResult::Cancelled;
        }
        let want =
            usize::try_from(size.saturating_sub(verified).min(CHUNK as u64)).unwrap_or(CHUNK);
        let Some(source_chunk) = source_buffer.get_mut(..want) else {
            return JobResult::VerifyFailed("source verify chunk exceeds buffer".into());
        };
        let Some(target_chunk) = target_buffer.get_mut(..want) else {
            return JobResult::VerifyFailed("target verify chunk exceeds buffer".into());
        };
        if let Err(error) = read_exact_fill(source, source_chunk) {
            return JobResult::VerifyFailed(error.to_string());
        }
        if let Err(error) = read_exact_fill(device, target_chunk) {
            return JobResult::VerifyFailed(error.to_string());
        }
        if let Some(offset) = first_diff(source_chunk, target_chunk) {
            return JobResult::VerifyMismatch(verified.saturating_add(offset as u64));
        }
        verified = verified.saturating_add(want as u64);
        on_progress(CopyProgress::Verifying {
            bytes: verified,
            total,
        });
    }

    JobResult::Verified
}
