//! What the bundle verification tests share: a signed (or unsigned) bundle built from a fixture
//! host root, and the policy a launch verifies it with.
#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "one shared fixture; each test binary uses the part it needs, and a failed unwrap or \
              index in a test is its assertion"
)]

use std::fs;
use std::path::{Path, PathBuf};

use runtime_bundle::{
    BUNDLE_MANIFEST_FILENAME, BuildOptions, BundleManifest, Identity, VerifyOptions,
    build_bundle_with,
};

pub const DYLIB: &str = "lib/libruntime_dylib.so";

pub struct Fixture {
    pub _work: tempfile::TempDir,
    pub bundle: PathBuf,
    pub publisher: String,
}

/// A bundle under `parent` whose toolkit dylib is `dylib_size` bytes, signed unless `signed` is
/// false. Two fixtures of different sizes hold different payloads under different root hashes.
pub fn signed_bundle(parent: &Path, dylib_size: usize, signed: bool) -> Fixture {
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

pub fn policy(publisher: &str) -> VerifyOptions {
    VerifyOptions {
        require_publisher: Some(publisher.to_string()),
        require_signature: true,
        require_arch: None,
    }
}

pub fn read_manifest(bundle: &Path) -> BundleManifest {
    toml::from_str(&fs::read_to_string(bundle.join(BUNDLE_MANIFEST_FILENAME)).unwrap()).unwrap()
}

pub fn write_manifest(bundle: &Path, manifest: &BundleManifest) {
    fs::write(
        bundle.join(BUNDLE_MANIFEST_FILENAME),
        toml::to_string_pretty(manifest).unwrap(),
    )
    .unwrap();
}

/// Replace the file at `path` with the same bytes but one, as anyone who can write the per-user
/// runtime directory can: the file itself may be immutable, its directory entry is not.
pub fn replace_with_one_byte_changed(path: &Path) {
    let mut bytes = fs::read(path).unwrap();
    bytes[0] ^= 1;
    fs::remove_file(path).unwrap();
    fs::write(path, bytes).unwrap();
}
