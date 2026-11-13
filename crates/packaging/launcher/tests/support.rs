use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use tilo_test_env::SpawnUnderEnvLock as _;

use launcher::manifest::{Entrypoint, Fs, ManifestView, Net, Sandbox};
use launcher::run::{IdentitySource, RunOptions};

/// A payload root holding `bin/hello`, the in-tree `hello` fixture.
pub fn hello_payload() -> &'static Path {
    static PAYLOAD: OnceLock<PathBuf> = OnceLock::new();
    built_once(&PAYLOAD, "hello", "2024")
}

/// A payload root holding `bin/workspacetest`, the in-tree `workspacetest` fixture.
pub fn workspacetest_payload() -> &'static Path {
    static PAYLOAD: OnceLock<PathBuf> = OnceLock::new();
    built_once(&PAYLOAD, "workspacetest", "2021")
}

/// Actual finite headless worker for signed capture-pipeline qualification.
pub fn capture_worker_payload() -> &'static Path {
    static PAYLOAD: OnceLock<PathBuf> = OnceLock::new();
    built_once(&PAYLOAD, "capture-worker", "2024")
}

/// The build runs `rustc` from `PATH`, which the sandbox-environment tests replace while they hold the
/// environment lock, so it runs under that lock too.
fn built_once(cell: &'static OnceLock<PathBuf>, name: &str, edition: &str) -> &'static Path {
    if let Some(payload) = cell.get() {
        return payload;
    }
    let _env = tilo_test_env::lock();
    cell.get_or_init(|| build_payload(name, edition))
}

/// `bwrap`, resolved on `PATH` rather than at a fixed path, since the distro decides where it lives.
pub fn bwrap() -> PathBuf {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let missing = format!(
        "bwrap is not on PATH ({}). It is how a Tilo app runs, so this is a broken environment \
         rather than a reason to skip the test.",
        path.to_string_lossy()
    );
    std::env::split_paths(&path)
        .map(|dir| dir.join("bwrap"))
        .find(|candidate| candidate.is_file())
        .expect(&missing)
}

/// A GUI package manifest naming one entrypoint and one network mode.
pub fn manifest(package_id: &str, bin: &str, net: &str) -> ManifestView {
    ManifestView {
        package_id: package_id.to_string(),
        name: None,
        localized_names: BTreeMap::default(),
        keywords: Vec::new(),
        category: None,
        icon: None,
        version: "0.0.0".to_string(),
        kind: "app".to_string(),
        runtime: None,
        required_runtimes: Vec::new(),
        entrypoint: Entrypoint {
            bin: bin.to_string(),
            r#type: launcher::manifest::EntrypointType::Gui,
            gui_bin: None,
            cli_bin: None,
            runtime: None,
            origins: Vec::new(),
        },
        sandbox: Sandbox {
            net: Net {
                mode: net.to_string(),
                ..Net::default()
            },
            fs: Fs::default(),
            services: BTreeMap::new(),
            portals: BTreeMap::new(),
            ..Sandbox::default()
        },
        cli_commands: Vec::new(),
        opens_ext: Vec::new(),
        opens_mime: Vec::new(),
        opens_schemes: Vec::new(),
        opens_secondary: false,
        continuity: launcher::manifest::ContinuityContract::default(),
        capabilities: Vec::new(),
    }
}

/// Launch options that touch nothing of the invoking user's: no identity is brokered and the
/// per-workspace app directories live under `data_home`.
pub fn isolated_options(data_home: &Path) -> RunOptions {
    RunOptions {
        identity: IdentitySource::None,
        data_home: Some(data_home.to_path_buf()),
        ..RunOptions::default()
    }
}

/// Compile `tests/fixtures/<name>/src/main.rs` with `rustc` into a payload root under
/// `CARGO_TARGET_TMPDIR`, once per source revision.
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
        .status_locked()
        .expect("invoke rustc");
    assert!(status.success(), "rustc failed to build the {name} fixture");
    // Another process may have finished the same build first; either copy is complete, so losing the
    // race is fine.
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

/// The compiler of the toolchain running these tests: `$RUSTC` when set, else the `rustc` beside the
/// `cargo` that launched the test binary, so a test environment whose `PATH` omits the toolchain still
/// finds it.
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
