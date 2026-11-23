//! What a launch leaves behind about a bundle it verified in full, so the next launch can trust the
//! same bytes without reading them.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failed unwrap or index in a test is its assertion"
)]

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use runtime_bundle::{BundleError, verify_bundle_recorded_with};

mod common;
use common::{DYLIB, policy, read_manifest, replace_with_one_byte_changed, signed_bundle};

fn records(tag: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(tag);
    fs::create_dir_all(&path).unwrap();
    (dir, path)
}

/// Set a file's modification time to `time`, which unprivileged code can do;
/// its change time still moves forward with the write.
fn set_modified(path: &Path, time: SystemTime) {
    fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(time)
        .unwrap();
}

tilo_test_env::scenario! {
    fn a_verification_record_answers_only_for_the_unchanged_bundle_it_verified;

    phase the_second_launch_trusts_the_record_instead_of_reading_the_payload;
    {
        let work = tempfile::tempdir().unwrap();
        let fixture = signed_bundle(work.path(), 4096, true);
        let files = read_manifest(&fixture.bundle).files.len();
        let policy = policy(&fixture.publisher);
        let (_dir, records) = records("verified");

        let (_, first) = verify_bundle_recorded_with(&fixture.bundle, &policy, Some(&records)).unwrap();
        assert_eq!(first.hashed, files, "the first launch proves every file");
        assert_eq!(first.by_record, 0);

        let (_, second) =
            verify_bundle_recorded_with(&fixture.bundle, &policy, Some(&records)).unwrap();
        assert_eq!(second.hashed, 0, "none of the payload is read again");
        assert_eq!(second.hashed_bytes, 0);
        assert_eq!(
            second.by_record, files,
            "the record covers the whole payload"
        );
    }

    phase a_record_never_covers_a_file_whose_bytes_changed;
    {
        let work = tempfile::tempdir().unwrap();
        let fixture = signed_bundle(work.path(), 4096, true);
        let policy = policy(&fixture.publisher);
        let (_dir, records) = records("verified");
        verify_bundle_recorded_with(&fixture.bundle, &policy, Some(&records)).unwrap();

        replace_with_one_byte_changed(&fixture.bundle.join(DYLIB));
        let refused = verify_bundle_recorded_with(&fixture.bundle, &policy, Some(&records));
        assert!(
            matches!(refused, Err(BundleError::HashMismatch { .. })),
            "a changed file is read and refused, got {refused:?}"
        );
    }

    phase restoring_a_files_modification_time_does_not_restore_its_place_in_the_record;
    {
        let work = tempfile::tempdir().unwrap();
        let fixture = signed_bundle(work.path(), 4096, true);
        let policy = policy(&fixture.publisher);
        let (_dir, records) = records("verified");
        verify_bundle_recorded_with(&fixture.bundle, &policy, Some(&records)).unwrap();

        let dylib = fixture.bundle.join(DYLIB);
        let modified = fs::metadata(&dylib).unwrap().modified().unwrap();
        replace_with_one_byte_changed(&dylib);
        set_modified(&dylib, modified);
        assert_eq!(
            fs::metadata(&dylib).unwrap().modified().unwrap(),
            modified,
            "the visible timestamp is what an attacker would restore"
        );
        let refused = verify_bundle_recorded_with(&fixture.bundle, &policy, Some(&records));
        assert!(
            matches!(refused, Err(BundleError::HashMismatch { .. })),
            "a replaced file keeps a different identity, got {refused:?}"
        );
    }

    phase a_record_answers_only_for_the_bundle_it_verified;
    {
        let work = tempfile::tempdir().unwrap();
        let one = signed_bundle(work.path(), 4096, true);
        let other = signed_bundle(work.path(), 8192, true);
        assert_ne!(
            read_manifest(&one.bundle).root_hash,
            read_manifest(&other.bundle).root_hash
        );
        let (_dir, records) = records("verified");
        verify_bundle_recorded_with(&one.bundle, &policy(&one.publisher), Some(&records)).unwrap();

        // Only the record's own root hash keeps one bundle's verification from
        // answering for another: the payload files are unrelated, and the
        // second bundle is verified in full.
        let files = read_manifest(&other.bundle).files.len();
        let (_, other_check) =
            verify_bundle_recorded_with(&other.bundle, &policy(&other.publisher), Some(&records))
                .unwrap();
        assert_eq!(other_check.by_record, 0);
        assert_eq!(other_check.hashed, files);
    }

    phase a_failed_verification_leaves_no_record;
    {
        let work = tempfile::tempdir().unwrap();
        let fixture = signed_bundle(work.path(), 4096, true);
        let policy = policy(&fixture.publisher);
        let (_dir, records) = records("verified");
        replace_with_one_byte_changed(&fixture.bundle.join(DYLIB));

        verify_bundle_recorded_with(&fixture.bundle, &policy, Some(&records)).unwrap_err();
        assert_eq!(
            fs::read_dir(&records).unwrap().count(),
            0,
            "nothing unverified is left for a launch to trust"
        );
    }

    phase an_unusable_record_directory_costs_verification_not_the_launch;
    {
        let work = tempfile::tempdir().unwrap();
        let fixture = signed_bundle(work.path(), 4096, true);
        let policy = policy(&fixture.publisher);
        let dir = tempfile::tempdir().unwrap();
        // A file where the record directory should be: both reading and writing
        // the record fail, and the launch still verifies.
        let not_a_directory = dir.path().join("verified");
        fs::write(&not_a_directory, b"").unwrap();

        let files = read_manifest(&fixture.bundle).files.len();
        let (_, check) =
            verify_bundle_recorded_with(&fixture.bundle, &policy, Some(&not_a_directory)).unwrap();
        assert_eq!(check.hashed, files);
    }
}
