use super::*;
use crate::media_model::SourceIdentity;
use std::io::Read;

fn request(
    source: &tempfile::NamedTempFile,
    size: u64,
    target_size: u64,
) -> DescriptorWriteRequest {
    DescriptorWriteRequest {
        source: ReviewedSource {
            path: source.path().into(),
            size,
            identity: SourceIdentity::from_file(source.as_file()).unwrap(),
        },
        expected_sha256: None,
        target_size,
        target_stable_id: String::new(),
        installable: true,
        provision_migration_data: false,
        has_legacy_handoff: false,
        #[cfg(target_os = "linux")]
        expected_layout: None,
    }
}

#[test]
fn held_descriptor_transaction_verifies_image_and_tail_profile() {
    let source = tempfile::NamedTempFile::new().unwrap();
    let mut target = tempfile::tempfile().unwrap();
    let image = vec![37; 8192];
    let profile = vec![62; PROFILE_BLOCK_SIZE];
    let capacity = image.len() as u64 + 2 * PROFILE_BLOCK_SIZE as u64;
    std::fs::write(source.path(), &image).unwrap();
    target.set_len(capacity).unwrap();
    let mut events = Vec::new();
    let result = run_job_file_bound(
        &request(&source, image.len() as u64, capacity),
        &mut target,
        &|| false,
        |event| events.push(event),
        |_| Ok(()),
        Ok,
        || Ok(profile.clone()),
    );
    assert_eq!(result, JobResult::Verified);
    target.seek(SeekFrom::Start(0)).unwrap();
    let mut copied = vec![0; image.len()];
    target.read_exact(&mut copied).unwrap();
    assert_eq!(copied, image);
    target
        .seek(SeekFrom::Start(capacity - PROFILE_BLOCK_SIZE as u64))
        .unwrap();
    let mut stored = vec![0; PROFILE_BLOCK_SIZE];
    target.read_exact(&mut stored).unwrap();
    assert_eq!(stored, profile);
    assert!(
        events
            .iter()
            .any(|event| matches!(event.phase, HelperPhase::Verifying))
    );
}

#[test]
fn cancellation_does_not_write_or_encode_profile() {
    let source = tempfile::NamedTempFile::new().unwrap();
    let mut target = tempfile::tempfile().unwrap();
    std::fs::write(source.path(), b"source").unwrap();
    target.write_all(b"preserve target").unwrap();
    let result = run_job_file_bound(
        &request(&source, 6, PROFILE_BLOCK_SIZE as u64),
        &mut target,
        &|| true,
        |_| panic!("cancelled job must not report copied bytes"),
        |_| Ok(()),
        Ok,
        || panic!("cancelled job must not encode or write a profile"),
    );
    assert_eq!(result, JobResult::Cancelled);
    target.seek(SeekFrom::Start(0)).unwrap();
    let mut actual = Vec::new();
    target.read_to_end(&mut actual).unwrap();
    assert_eq!(actual, b"preserve target");
}
