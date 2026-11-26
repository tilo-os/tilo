//! Where the Chromium engine lives. On Linux the engine — the `tilo-chromium-host`
//! process and the Chromium Embedded Framework it loads — is its own signed Tilo runtime
//! bundle, [`RUNTIME_REF`].

use std::ffi::OsString;
use std::path::PathBuf;

/// The engine runtime's version: the directory name of its bundle and of its mount.
pub const RUNTIME_VERSION: &str = "2026.10.23-chromium";

/// The runtime reference an app that embeds the engine requires in its manifest's
/// `[package].optional-runtimes`.
pub const RUNTIME_REF: &str = "tilo-runtime/2026.10.23-chromium";

/// The host executable, relative to the bundle root.
pub const HOST_IN_BUNDLE: &str = "lib/chromium/tilo-chromium-host";

/// Names a host executable directly, for a developer running a locally built engine.
/// Release images never set it.
pub const HOST_OVERRIDE_ENV: &str = "TILO_CHROMIUM_HOST";

/// Overrides the per-user runtime root, as it does for every runtime bundle.
pub const RUNTIME_DIR_ENV: &str = "TILO_RUNTIME_BUNDLE_DIR";

/// Where the launcher mounts runtime bundles inside an app's sandbox.
pub const MOUNT_ROOT: &str = "/run/tilo-runtime";

/// Where the image installs system runtime bundles.
pub const SYSTEM_ROOT: &str = "/usr/lib/tilo-runtime";

/// No engine runtime was found in any place it is installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeMissing {
    pub searched: Vec<PathBuf>,
}

impl RuntimeMissing {
    /// What a developer can do about it.
    #[must_use]
    pub fn developer_hint(&self) -> String {
        let searched = self
            .searched
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "Chromium engine runtime {RUNTIME_VERSION} is not installed (looked for {searched}). \
             Images install it at {SYSTEM_ROOT}/{RUNTIME_VERSION}; for a local build run \
             `python3 scripts/browser/build-host.py` and set {HOST_OVERRIDE_ENV} to \
             target/chromium/tilo-chromium-host."
        )
    }
}

/// What a person sees when the engine is missing.
pub const NOT_INSTALLED_MESSAGE: &str = "The browser engine is missing.";

/// What a person sees when an installed engine fails to start.
pub const FAILED_MESSAGE: &str = "The browser engine couldn’t start.";

/// The places the engine is looked for, in order.
#[derive(Debug, Clone, Default)]
pub struct SearchRoots {
    pub host_override: Option<PathBuf>,
    pub mount_root: Option<PathBuf>,
    pub user_root: Option<PathBuf>,
    pub system_root: Option<PathBuf>,
    pub executable: Option<PathBuf>,
}

impl SearchRoots {
    /// The roots of the running process.
    #[must_use]
    pub fn current() -> Self {
        Self {
            host_override: non_empty(std::env::var_os(HOST_OVERRIDE_ENV)).map(PathBuf::from),
            mount_root: Some(PathBuf::from(MOUNT_ROOT)),
            user_root: non_empty(std::env::var_os(RUNTIME_DIR_ENV))
                .map(PathBuf::from)
                .or_else(|| tilo_paths::data_dir().map(|dir| dir.join("tilo-runtime"))),
            system_root: Some(PathBuf::from(SYSTEM_ROOT)),
            executable: std::env::current_exe().ok(),
        }
    }

    /// Every host path, in the order they are tried.
    #[must_use]
    pub fn candidates(&self) -> Vec<PathBuf> {
        let mut candidates = Vec::new();
        candidates.extend(self.host_override.clone());
        for root in [&self.mount_root, &self.user_root, &self.system_root]
            .into_iter()
            .flatten()
        {
            candidates.push(root.join(RUNTIME_VERSION).join(HOST_IN_BUNDLE));
        }
        candidates.extend(browser_continuity_core::engine_host::bundled_host(
            self.executable.as_deref(),
        ));
        candidates
    }

    pub fn locate(&self) -> Result<PathBuf, RuntimeMissing> {
        let candidates = self.candidates();
        candidates
            .iter()
            .find(|path| path.is_file())
            .cloned()
            .ok_or(RuntimeMissing {
                searched: candidates,
            })
    }
}

fn non_empty(value: Option<OsString>) -> Option<OsString> {
    value.filter(|value| !value.is_empty())
}

/// The engine host of the running process.
pub fn locate() -> Result<PathBuf, RuntimeMissing> {
    SearchRoots::current().locate()
}
