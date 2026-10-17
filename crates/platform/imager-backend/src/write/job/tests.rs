use super::*;
use crate::write::CHUNK;
use sha2::{Digest, Sha256};

#[test]
fn native_path_transaction_copies_verifies_and_reports_phases() {
    let source = tempfile::NamedTempFile::new().unwrap();
    let target = tempfile::NamedTempFile::new().unwrap();
    let bytes = vec![91_u8; CHUNK + 19];
    std::fs::write(source.path(), &bytes).unwrap();
    let mut events = Vec::new();
    let result = run_job_paths_bound(
        source.path(),
        target.path(),
        bytes.len() as u64,
        SourceIdentity::from_file(source.as_file()).unwrap(),
        Some(&hex::encode(Sha256::digest(&bytes))),
        &|| false,
        |progress| events.push(progress),
    );
    assert_eq!(result, JobResult::Verified);
    assert_eq!(std::fs::read(target.path()).unwrap(), bytes);
    assert!(
        events
            .iter()
            .any(|event| matches!(event.phase, HelperPhase::Writing))
    );
    let last = events.last().unwrap();
    assert!(matches!(last.phase, HelperPhase::Verifying));
    assert_eq!(last.verify_frac, 1.0);
}

#[test]
fn signed_digest_mismatch_never_opens_destination_for_writing() {
    let source = tempfile::NamedTempFile::new().unwrap();
    let target = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(source.path(), b"changed source").unwrap();
    std::fs::write(target.path(), b"untouched target").unwrap();
    let result = run_job_paths_bound(
        source.path(),
        target.path(),
        14,
        None,
        Some(&"0".repeat(64)),
        &|| false,
        |_| {},
    );
    assert!(
        matches!(result, JobResult::WriteFailed(message) if message.contains("signed release catalog"))
    );
    assert_eq!(std::fs::read(target.path()).unwrap(), b"untouched target");
}
