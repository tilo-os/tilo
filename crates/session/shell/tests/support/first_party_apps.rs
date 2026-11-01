//! Tilo's own apps installed in a test session's app store, as the session
//! runs them: their own processes, started through the real `tilo-launcher`
//! in its sandbox, each window bound into the tab that asked for it.
//!
//! The compositor draws no app itself, so a test of an app tab needs the app
//! installed. [`install`] builds the app's binary for this run (once per run,
//! [`tilo_test_env::sibling_binary`]), stages it as a package payload beside
//! the build, and names it in the store's registry with the manifest the app
//! ships (`apps/<app>/tilo-app.toml`). The launcher the compositor starts
//! apps with is the run's own build of it ([`launcher`]).
//!
//! The binary is the one Cargo built, linked against this machine's
//! libraries, so the package declares no runtime bundle: the launcher binds
//! the system's loader and libraries instead, as it does for any app without
//! one.

use std::fmt::Write as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};

/// One of Tilo's own apps: its directory under `apps/`, the Cargo package
/// and binary that build it, and its package id.
#[derive(Clone, Copy, Debug)]
pub struct FirstPartyApp {
    pub dir: &'static str,
    pub package: &'static str,
    pub bin: &'static str,
    pub id: &'static str,
}

pub const NOTES: FirstPartyApp = FirstPartyApp {
    dir: "notes",
    package: "notes",
    bin: "notes",
    id: "so.tilo.notes",
};

#[allow(
    dead_code,
    reason = "the idle-wakeups example shares this module and starts only Notes"
)]
pub const FILES: FirstPartyApp = FirstPartyApp {
    dir: "file-explorer",
    package: "file-explorer",
    bin: "file-explorer",
    id: "so.tilo.file-explorer",
};

/// Tasks, where an agent's work through the door is a task of its own.
#[allow(
    dead_code,
    reason = "the idle-wakeups example shares this module and starts only Notes"
)]
pub const TASKS: FirstPartyApp = FirstPartyApp {
    dir: "tasks",
    package: "tasks",
    bin: "tasks",
    id: "so.tilo.tasks",
};

/// The run's own `tilo-launcher`, which the compositor is pointed at
/// (`TILO_LAUNCHER_BIN`) to start the installed apps.
pub fn launcher() -> PathBuf {
    tilo_test_env::sibling_binary("launcher", "tilo-launcher")
}

/// Install `apps` in the app store at `store` (a session's
/// `$XDG_DATA_HOME/tilo-apps`): each app's payload and manifest, then the
/// registry naming them all, written whole as an install does.
pub fn install(store: &Path, apps: &[FirstPartyApp]) {
    let mut registry = String::new();
    for app in apps {
        let payload = payload(app);
        let manifest = store.join("manifests").join(format!("{}.toml", app.id));
        std::fs::create_dir_all(manifest.parent().expect("a manifest directory"))
            .expect("create the store's manifest directory");
        std::fs::write(&manifest, manifest_without_runtime(app))
            .unwrap_or_else(|error| panic!("write {}: {error}", manifest.display()));
        write!(
            registry,
            "[[entry]]\nid = {:?}\npayload_root = {:?}\nmanifest_path = {:?}\n",
            app.id,
            payload.display().to_string(),
            manifest.display().to_string(),
        )
        .expect("format a registry entry");
    }
    let registry_path = package_store::Layout::new(store).registry();
    std::fs::create_dir_all(registry_path.parent().expect("a registry directory"))
        .expect("create the app store");
    let staged = registry_path.with_extension("toml.tmp");
    std::fs::write(&staged, registry).expect("stage the registry");
    std::fs::rename(&staged, &registry_path).expect("install the registry");
}

/// The manifest `app` ships, without its `runtime`: the binary this run
/// built links the machine's own libraries, not a runtime bundle's.
fn manifest_without_runtime(app: &FirstPartyApp) -> String {
    let shipped = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../apps")
        .join(app.dir)
        .join("tilo-app.toml");
    let text = std::fs::read_to_string(&shipped)
        .unwrap_or_else(|error| panic!("read {}: {error}", shipped.display()));
    text.lines()
        .filter(|line| {
            !line
                .split_once('=')
                .is_some_and(|(key, _)| key.trim() == "runtime")
        })
        .fold(String::new(), |mut out, line| {
            out.push_str(line);
            out.push('\n');
            out
        })
}

/// `app`'s payload root: its binary at the manifest's `bin/<name>`, beside
/// the build it came from so it is a hard link rather than a copy, keyed by
/// that build's file so a rebuild stages anew. Every test process of a run
/// shares it; one that finds it staged uses it as it is.
fn payload(app: &FirstPartyApp) -> PathBuf {
    let binary = tilo_test_env::sibling_binary(app.package, app.bin);
    let built = std::fs::metadata(&binary)
        .unwrap_or_else(|error| panic!("stat {}: {error}", binary.display()));
    let root = binary
        .parent()
        .expect("a binary has a directory")
        .join("tilo-test-payloads")
        .join(app.id);
    let payload = root.join(format!("{}-{}", built.ino(), built.mtime_nsec()));
    if payload.join("bin").join(app.bin).is_file() {
        return payload;
    }
    std::fs::create_dir_all(&root).expect("create the payload directory");
    let staging = tempfile::Builder::new()
        .prefix(".staging-")
        .tempdir_in(&root)
        .expect("create a payload staging directory");
    let bin = staging.path().join("bin");
    std::fs::create_dir_all(&bin).expect("create the payload's bin");
    let staged = bin.join(app.bin);
    if std::fs::hard_link(&binary, &staged).is_err() {
        std::fs::copy(&binary, &staged)
            .unwrap_or_else(|error| panic!("copy {}: {error}", binary.display()));
    }
    let surface = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../apps")
        .join(app.dir)
        .join("surface.toml");
    if surface.is_file() {
        std::fs::copy(&surface, staging.path().join("surface.toml"))
            .expect("stage the app's surface");
    }
    let staging = staging.keep();
    // Another test process may have staged the same build meanwhile: its
    // payload is this one, so either rename leaves a complete payload.
    if std::fs::rename(&staging, &payload).is_err() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    assert!(
        payload.join("bin").join(app.bin).is_file(),
        "no staged payload at {}",
        payload.display()
    );
    payload
}
