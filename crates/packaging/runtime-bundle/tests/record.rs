//! What a launch leaves behind about a bundle it verified in full, so the next launch can trust the
//! same bytes without reading them.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use runtime_bundle::{
    BUNDLE_MANIFEST_FILENAME, BuildOptions, BundleError, BundleManifest, Identity, VerifyOptions,
    build_bundle_with, verify_bundle_recorded_with,
};

const DYLIB: &str = "lib/libruntime_dylib.so";

struct Fixture {
    _work: tempfile::TempDir,
    bundle: PathBuf,
    publisher: String,
}

/// A signed bundle under `parent` whose toolkit dylib is `dylib_size` bytes,
/// so two fixtures hold different payloads under different root hashes.
fn signed_bundle(parent: &Path, dylib_size: usize) -> Fixture {
    let work = tempfile::tempdir_in(parent).unwrap();
    let host = work.path().join("host");
    for rel in ["lib64/ld-linux-x86-64.so.2", "lib64/libc.so.6"] {
        let path = host.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, format!("STUB: {rel}\n")).unwrap();
    }
    let dylib = work.path().join("libruntime_dylib.so");
    let bytes: Vec<u8> = (0..=250u8).cycle().take(dylib_size).collect();
    fs::write(&dylib, bytes).unwrap();
    let identity = Identity::generate();
    let identity_path = work.path().join("id.json");
    fs::write(&identity_path, identity.to_json_bytes()).unwrap();
    let bundle = work.path().join("bundle");
    build_bundle_with(
        &bundle,
        "2026.10",
        &BuildOptions {
            from: Some(&host),
            arch: Some("x86_64"),
            identity: Some(identity_path.as_path()),
            extra_libs: &[dylib],
            extra_bins: &[],
        },
    )
    .unwrap();
    Fixture {
        _work: work,
        bundle,
        publisher: identity.public_key_hex(),
    }
}

fn policy(publisher: &str) -> VerifyOptions {
    VerifyOptions {
        require_publisher: Some(publisher.to_string()),
        require_signature: true,
        require_arch: None,
    }
}

fn read_manifest(bundle: &Path) -> BundleManifest {
    toml::from_str(&fs::read_to_string(bundle.join(BUNDLE_MANIFEST_FILENAME)).unwrap()).unwrap()
}

fn records(tag: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(tag);
    fs::create_dir_all(&path).unwrap();
    (dir, path)
}

/// Replace the file at `path` with content one byte different, as anyone who
/// can write the per-user runtime directory can.
fn replace_with_one_byte_changed(path: &Path) {
    let mut bytes = fs::read(path).unwrap();
    bytes[0] ^= 1;
    fs::remove_file(path).unwrap();
    fs::write(path, bytes).unwrap();
}

#[test]
fn the_second_launch_trusts_the_record_instead_of_reading_the_payload() {
    let work = tempfile::tempdir().unwrap();
    let fixture = signed_bundle(work.path(), 4096);
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

#[test]
fn a_record_never_covers_a_file_whose_bytes_changed() {
    let work = tempfile::tempdir().unwrap();
    let fixture = signed_bundle(work.path(), 4096);
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

#[test]
fn restoring_a_files_modification_time_does_not_restore_its_place_in_the_record() {
    let work = tempfile::tempdir().unwrap();
    let fixture = signed_bundle(work.path(), 4096);
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

#[test]
fn a_record_answers_only_for_the_bundle_it_verified() {
    let work = tempfile::tempdir().unwrap();
    let one = signed_bundle(work.path(), 4096);
    let other = signed_bundle(work.path(), 8192);
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

#[test]
fn a_failed_verification_leaves_no_record() {
    let work = tempfile::tempdir().unwrap();
    let fixture = signed_bundle(work.path(), 4096);
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

#[test]
fn an_unusable_record_directory_costs_verification_not_the_launch() {
    let work = tempfile::tempdir().unwrap();
    let fixture = signed_bundle(work.path(), 4096);
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
