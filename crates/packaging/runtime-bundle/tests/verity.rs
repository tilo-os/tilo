//! fs-verity on runtime bundles: the builder signs each file's digest, a
//! launch trusts a sealed file unread, and anything short of a sealed file
//! with the signed digest is hashed and refused if a byte differs.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use runtime_bundle::{
    BUNDLE_MANIFEST_FILENAME, BuildOptions, BundleError, BundleManifest, Identity, PayloadCheck,
    VerifyOptions, build_bundle_with, enable_bundle_verity, verify_bundle_verity_with,
    verify_bundle_with, verity,
};
use tilo_lock::MutexRecover as _;
use tilo_test_env::require_capability;

/// The payload-read assertion measures the whole process's reads, so the
/// tests here, which all read files, run one at a time.
static READS: Mutex<()> = Mutex::new(());

fn one_at_a_time() -> std::sync::MutexGuard<'static, ()> {
    READS.lock_recover()
}

const DYLIB: &str = "lib/libruntime_dylib.so";

struct Fixture {
    _work: tempfile::TempDir,
    bundle: PathBuf,
    publisher: String,
}

/// A signed bundle under `parent` whose toolkit dylib is `dylib_size` bytes.
fn signed_bundle(parent: &Path, dylib_size: usize, signed: bool) -> Fixture {
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
            identity: signed.then_some(identity_path.as_path()),
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

fn write_manifest(bundle: &Path, manifest: &BundleManifest) {
    fs::write(
        bundle.join(BUNDLE_MANIFEST_FILENAME),
        toml::to_string_pretty(manifest).unwrap(),
    )
    .unwrap();
}

/// Replace the file at `path` with the same bytes but one, as anyone who can
/// write the per-user runtime directory can: the file itself may be immutable,
/// its directory entry is not.
fn replace_with_one_byte_changed(path: &Path) {
    let mut bytes = fs::read(path).unwrap();
    bytes[0] ^= 1;
    fs::remove_file(path).unwrap();
    fs::write(path, bytes).unwrap();
}

/// A temporary directory on a filesystem with fs-verity: under
/// `TILO_FSVERITY_DIR`, else the target's temporary directory.
fn verity_dir() -> Result<tempfile::TempDir, String> {
    let base = std::env::var_os("TILO_FSVERITY_DIR")
        .filter(|dir| !dir.is_empty())
        .map_or_else(|| PathBuf::from(env!("CARGO_TARGET_TMPDIR")), PathBuf::from);
    fs::create_dir_all(&base).map_err(|e| format!("{}: {e}", base.display()))?;
    let dir = tempfile::tempdir_in(&base).map_err(|e| format!("{}: {e}", base.display()))?;
    let probe = dir.path().join("probe");
    fs::write(&probe, b"probe").map_err(|e| e.to_string())?;
    verity::enable(&probe).map_err(|e| format!("{}: {e}", base.display()))?;
    Ok(dir)
}

/// Bytes this process has read through `read(2)` and friends.
#[cfg(target_os = "linux")]
fn bytes_read() -> u64 {
    fs::read_to_string("/proc/self/io")
        .unwrap()
        .lines()
        .find_map(|line| line.strip_prefix("rchar: "))
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

#[test]
fn the_builder_signs_each_files_verity_digest_apart_from_the_root_hash() {
    let _serial = one_at_a_time();
    let fixture = signed_bundle(&std::env::temp_dir(), 3 * 4096 + 5, true);
    let manifest = verify_bundle_with(&fixture.bundle, &policy(&fixture.publisher)).unwrap();
    assert!(manifest.verity_signature_hex.is_some());
    for file in &manifest.files {
        let bytes = fs::read(fixture.bundle.join(&file.path)).unwrap();
        assert_eq!(
            file.verity.as_deref(),
            Some(hex::encode(verity::digest(&bytes)).as_str()),
            "{}",
            file.path
        );
    }

    // phase: a_forged_digest_breaks_the_verity_signature
    let mut forged = manifest.clone();
    forged.files[0].verity = Some(hex::encode(verity::digest(b"other bytes")));
    write_manifest(&fixture.bundle, &forged);
    for result in [
        verify_bundle_with(&fixture.bundle, &policy(&fixture.publisher)).map(|_| ()),
        verify_bundle_verity_with(&fixture.bundle, &policy(&fixture.publisher)).map(|_| ()),
    ] {
        assert!(
            matches!(result, Err(BundleError::BadVeritySignature)),
            "{result:?}"
        );
    }

    // phase: the_digests_change_neither_the_root_hash_nor_the_signature
    // A launcher that predates fs-verity ignores both fields; the bundle must
    // still verify there, so stripping them leaves a valid signed bundle.
    let mut stripped = manifest.clone();
    stripped.verity_signature_hex = None;
    for file in &mut stripped.files {
        file.verity = None;
    }
    write_manifest(&fixture.bundle, &stripped);
    let verified = verify_bundle_with(&fixture.bundle, &policy(&fixture.publisher)).unwrap();
    assert_eq!(verified.root_hash, manifest.root_hash);
    let (_, check) =
        verify_bundle_verity_with(&fixture.bundle, &policy(&fixture.publisher)).unwrap();
    assert_eq!(check.by_verity, 0);
    assert_eq!(check.hashed, manifest.files.len());
}

#[test]
fn without_fs_verity_the_launch_check_hashes_and_refuses_a_modified_byte() {
    let _serial = one_at_a_time();
    let fixture = signed_bundle(&std::env::temp_dir(), 2 * 4096, true);
    let (manifest, check) =
        verify_bundle_verity_with(&fixture.bundle, &policy(&fixture.publisher)).unwrap();
    let total: u64 = manifest.files.iter().map(|file| file.size).sum();
    assert_eq!(
        check,
        PayloadCheck {
            by_verity: 0,
            hashed: manifest.files.len(),
            hashed_bytes: total,
        }
    );
    replace_with_one_byte_changed(&fixture.bundle.join(DYLIB));
    let refused = verify_bundle_verity_with(&fixture.bundle, &policy(&fixture.publisher));
    assert!(
        matches!(refused, Err(BundleError::HashMismatch { .. })),
        "{refused:?}"
    );
}

#[test]
fn an_unsigned_bundle_is_always_hashed() {
    let _serial = one_at_a_time();
    let Ok(dir) = verity_dir() else {
        // Sealed or not, an unsigned bundle's digests vouch for nothing; on a
        // filesystem without fs-verity there is nothing more to show.
        let fixture = signed_bundle(&std::env::temp_dir(), 4096, false);
        let (manifest, check) =
            verify_bundle_verity_with(&fixture.bundle, &VerifyOptions::default()).unwrap();
        assert!(manifest.verity_signature_hex.is_none());
        assert_eq!(check.hashed, manifest.files.len());
        return;
    };
    let fixture = signed_bundle(dir.path(), 4096, false);
    let manifest = read_manifest(&fixture.bundle);
    assert!(manifest.verity_signature_hex.is_none());
    assert_eq!(
        enable_bundle_verity(&fixture.bundle, &manifest).unwrap(),
        manifest.files.len()
    );
    let (_, check) = verify_bundle_verity_with(&fixture.bundle, &VerifyOptions::default()).unwrap();
    assert_eq!(check.by_verity, 0);
    assert_eq!(check.hashed, manifest.files.len());
}

#[test]
fn the_kernel_measures_the_digest_the_builder_signs() {
    let _serial = one_at_a_time();
    let dir = require_capability!(
        FsVerity,
        verity_dir(),
        "no fs-verity filesystem to measure on"
    );
    for size in [
        0,
        1,
        4095,
        4096,
        4097,
        128 * 4096,
        128 * 4096 + 1,
        129 * 4096 + 7,
    ] {
        let path = dir.path().join(format!("file-{size}"));
        let bytes = vec![0x5a; size];
        fs::write(&path, &bytes).unwrap();
        verity::enable(&path).unwrap();
        let measured = verity::measure(&fs::File::open(&path).unwrap());
        assert_eq!(measured, Some(verity::digest(&bytes)), "size {size}");
    }
}

#[test]
fn a_sealed_bundle_verifies_unread_and_still_refuses_a_modified_byte() {
    let _serial = one_at_a_time();
    let dir = require_capability!(
        FsVerity,
        verity_dir(),
        "no fs-verity filesystem to seal a bundle on"
    );
    let fixture = signed_bundle(dir.path(), 64 << 20, true);
    let policy = policy(&fixture.publisher);
    let manifest = verify_bundle_with(&fixture.bundle, &policy).unwrap();
    assert_eq!(
        enable_bundle_verity(&fixture.bundle, &manifest).unwrap(),
        manifest.files.len()
    );

    // phase: verified_without_reading_the_payload
    #[cfg(target_os = "linux")]
    let before = bytes_read();
    let started = std::time::Instant::now();
    let (_, check) = verify_bundle_verity_with(&fixture.bundle, &policy).unwrap();
    let elapsed = started.elapsed();
    #[cfg(target_os = "linux")]
    {
        let read = bytes_read().saturating_sub(before);
        assert!(read < 64 * 1024, "read {read} bytes of a 64 MiB bundle");
    }
    assert_eq!(
        check,
        PayloadCheck {
            by_verity: manifest.files.len(),
            hashed: 0,
            hashed_bytes: 0,
        }
    );
    eprintln!("sealed 64 MiB bundle verified in {elapsed:?}");

    // phase: a_sealed_file_cannot_be_written
    let dylib = fixture.bundle.join(DYLIB);
    assert!(fs::OpenOptions::new().write(true).open(&dylib).is_err());

    // phase: a_replaced_file_is_hashed_and_refused
    replace_with_one_byte_changed(&dylib);
    let refused = verify_bundle_verity_with(&fixture.bundle, &policy);
    assert!(
        matches!(refused, Err(BundleError::HashMismatch { .. })),
        "{refused:?}"
    );

    // phase: a_replaced_file_sealed_by_the_attacker_is_refused_too
    verity::enable(&dylib).unwrap();
    let refused = verify_bundle_verity_with(&fixture.bundle, &policy);
    assert!(
        matches!(refused, Err(BundleError::HashMismatch { .. })),
        "{refused:?}"
    );
}
