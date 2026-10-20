//! Shared fixtures for the launcher integration tests.

use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use launcher::run::{IdentitySource, RunOptions};

/// A payload root holding `bin/hello`, the in-tree `hello` fixture.
pub fn hello_payload() -> &'static Path {
    static PAYLOAD: OnceLock<PathBuf> = OnceLock::new();
    built_once(&PAYLOAD, "hello", "2024")
}

/// A payload root holding `bin/roomtest`, the in-tree `roomtest` fixture.
pub fn roomtest_payload() -> &'static Path {
    static PAYLOAD: OnceLock<PathBuf> = OnceLock::new();
    built_once(&PAYLOAD, "roomtest", "2021")
}

/// Actual finite headless worker for signed capture-pipeline qualification.
pub fn capture_worker_payload() -> &'static Path {
    static PAYLOAD: OnceLock<PathBuf> = OnceLock::new();
    built_once(&PAYLOAD, "capture-worker", "2024")
}

/// The build runs `rustc` from `PATH`, which the sandbox-environment tests
/// replace while they hold the environment lock, so it runs under that lock
/// too. The lock is taken *before* the `OnceLock`: a test that already holds
/// it and asks for a payload then finds either a finished build or an idle
/// initialiser, never one blocked on the lock it holds.
fn built_once(cell: &'static OnceLock<PathBuf>, name: &str, edition: &str) -> &'static Path {
    if let Some(payload) = cell.get() {
        return payload;
    }
    let _env = tilo_test_env::lock();
    cell.get_or_init(|| build_payload(name, edition))
}

/// Launch options that touch nothing of the invoking user's: no identity is
/// brokered and the per-room app directories live under `data_home`.
pub fn isolated_options(data_home: &Path) -> RunOptions {
    RunOptions {
        identity: IdentitySource::None,
        data_home: Some(data_home.to_path_buf()),
        ..RunOptions::default()
    }
}

/// Compile `tests/fixtures/<name>/src/main.rs` with `rustc` into a payload
/// root under `CARGO_TARGET_TMPDIR`, once per source revision.
///
/// The fixture lives outside the workspace, so it is built with a single
/// `rustc` call rather than `cargo`. The result is keyed by a hash of the
/// source and reused by every test and every later test process; a process
/// running tests one per process (nextest) finds it already built. A binary
/// written by the `rustc` child is never held open for writing by this
/// process, so executing it cannot fail with `ETXTBSY`.
fn build_payload(name: &str, edition: &str) -> PathBuf {
    let src = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
        .join("src/main.rs");
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    std::fs::read(&src)
        .expect("read fixture source")
        .hash(&mut hasher);
    edition.hash(&mut hasher);
    let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join("launcher-fixtures");
    let payload = root.join(format!("{name}-{:016x}", hasher.finish()));
    if payload.join("bin").join(name).is_file() {
        return payload;
    }

    std::fs::create_dir_all(&root).expect("create fixture root");
    let staging = tempfile::Builder::new()
        .prefix(&format!(".{name}-"))
        .tempdir_in(&root)
        .expect("create fixture staging dir");
    let bin = staging.path().join("bin").join(name);
    std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
    let status = std::process::Command::new(rustc())
        .arg(format!("--edition={edition}"))
        .arg("-O")
        .arg(&src)
        .arg("-o")
        .arg(&bin)
        .status()
        .expect("invoke rustc");
    assert!(status.success(), "rustc failed to build the {name} fixture");
    // Another process may have finished the same build first; either copy
    // is complete, so losing the race is fine.
    let staged = staging.keep();
    if std::fs::rename(&staged, &payload).is_err() {
        let _ = std::fs::remove_dir_all(&staged);
        assert!(
            payload.join("bin").join(name).is_file(),
            "the {name} fixture is neither built here nor by a concurrent build"
        );
    }
    payload
}

/// The compiler of the toolchain running these tests: `$RUSTC` when set, else
/// the `rustc` beside the `cargo` that launched the test binary, so a test
/// environment whose `PATH` omits the toolchain still finds it.
fn rustc() -> std::path::PathBuf {
    if let Some(rustc) = std::env::var_os("RUSTC") {
        return rustc.into();
    }
    std::env::var_os("CARGO")
        .map(std::path::PathBuf::from)
        .map(|cargo| cargo.with_file_name("rustc"))
        .filter(|rustc| rustc.is_file())
        .unwrap_or_else(|| "rustc".into())
}

/// `tilo-launcher` as built for these tests.
pub fn launcher_binary() -> PathBuf {
    package_binary("tilo-launcher", env!("CARGO_BIN_EXE_tilo-launcher"))
}

/// A binary of this package. An archived nextest run relocates the binaries
/// away from where Cargo built them and names their new place in
/// `NEXTEST_BIN_EXE_<name>`; a plain `cargo test` leaves them at `built_at`.
pub fn package_binary(name: &str, built_at: &str) -> PathBuf {
    std::env::var_os(format!("NEXTEST_BIN_EXE_{name}"))
        .map_or_else(|| PathBuf::from(built_at), PathBuf::from)
}
