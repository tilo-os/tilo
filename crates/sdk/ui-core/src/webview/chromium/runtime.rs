//! Where the Chromium engine lives.
//!
//! On Linux the engine — the `tilo-chromium-host` process and the Chromium
//! Embedded Framework it loads — is its own signed Tilo runtime bundle,
//! [`RUNTIME_REF`]. It is installed the way every runtime bundle is: by the
//! image at `/usr/lib/tilo-runtime/<version>`, or for one user under the
//! per-user runtime directory. An app that embeds the engine names the bundle
//! in its manifest's `[package].optional-runtimes`, and the launcher verifies
//! the bundle's signature and mounts it read-only at
//! `/run/tilo-runtime/<version>` inside the app's sandbox. Outside the
//! launcher the installed bundle is used directly.
//!
//! On macOS the packaged app carries the engine as a nested application in
//! its `Contents/Frameworks` directory.
//!
//! A missing engine is an ordinary state: [`locate`] reports it as
//! [`RuntimeMissing`], which an app shows as "not installed" and logs with
//! [`RuntimeMissing::developer_hint`].

use std::ffi::OsString;
use std::path::PathBuf;

/// The engine runtime's version: the directory name of its bundle and of its
/// mount. Bump it whenever the host's protocol or its pinned CEF build
/// changes, or when a host repair must reach an already installed bundle.
/// The Store keeps a verified bundle already present at this version.
pub const RUNTIME_VERSION: &str = "2026.10.17-chromium";

/// The runtime reference an app manifest lists in
/// `[package].optional-runtimes`.
pub const RUNTIME_REF: &str = "tilo-runtime/2026.10.17-chromium";

/// The host executable, relative to the bundle root.
pub const HOST_IN_BUNDLE: &str = "lib/chromium/tilo-chromium-host";

/// Names a host executable directly, for a developer running a locally
/// built engine. Release images never set it.
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
    /// Every host path that was checked, in order.
    pub searched: Vec<PathBuf>,
}

impl RuntimeMissing {
    /// What a developer can do about it. For logs only; users see
    /// [`NOT_INSTALLED_MESSAGE`].
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

/// What a person sees when the engine is not installed.
pub const NOT_INSTALLED_MESSAGE: &str = "The browser engine isn’t installed yet.";

/// What a person sees when an installed engine fails to start. The cause goes
/// to the log.
pub const FAILED_MESSAGE: &str = "The browser engine couldn’t start.";

/// The places the engine is looked for, in order.
#[derive(Debug, Clone, Default)]
pub struct SearchRoots {
    /// [`HOST_OVERRIDE_ENV`].
    pub host_override: Option<PathBuf>,
    /// The launcher's mount root ([`MOUNT_ROOT`]).
    pub mount_root: Option<PathBuf>,
    /// The per-user runtime root.
    pub user_root: Option<PathBuf>,
    /// The image's runtime root ([`SYSTEM_ROOT`]).
    pub system_root: Option<PathBuf>,
    /// The running executable, for the macOS app layout.
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

    /// The first installed host.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeMissing`] naming every place checked when none holds
    /// the host.
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
///
/// # Errors
///
/// Returns [`RuntimeMissing`] when the engine runtime is not installed.
pub fn locate() -> Result<PathBuf, RuntimeMissing> {
    SearchRoots::current().locate()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn install(root: &Path) -> PathBuf {
        let host = root.join(RUNTIME_VERSION).join(HOST_IN_BUNDLE);
        std::fs::create_dir_all(host.parent().unwrap()).unwrap();
        std::fs::write(&host, b"host").unwrap();
        host
    }

    fn roots(base: &Path) -> SearchRoots {
        SearchRoots {
            host_override: None,
            mount_root: Some(base.join("mount")),
            user_root: Some(base.join("user")),
            system_root: Some(base.join("system")),
            executable: Some(base.join("app/bin/browser")),
        }
    }

    #[test]
    fn the_runtime_reference_names_the_bundle_version() {
        assert_eq!(
            RUNTIME_REF.split_once('/'),
            Some(("tilo-runtime", RUNTIME_VERSION))
        );
    }

    #[test]
    fn a_missing_engine_names_every_place_it_looked() {
        let base = tempfile::tempdir().unwrap();
        let missing = roots(base.path()).locate().unwrap_err();
        assert_eq!(
            &missing.searched[..3],
            &[
                base.path()
                    .join("mount")
                    .join(RUNTIME_VERSION)
                    .join(HOST_IN_BUNDLE),
                base.path()
                    .join("user")
                    .join(RUNTIME_VERSION)
                    .join(HOST_IN_BUNDLE),
                base.path()
                    .join("system")
                    .join(RUNTIME_VERSION)
                    .join(HOST_IN_BUNDLE),
            ]
        );
        let hint = missing.developer_hint();
        assert!(hint.contains(RUNTIME_VERSION) && hint.contains(HOST_OVERRIDE_ENV));
        assert!(!NOT_INSTALLED_MESSAGE.contains("build-host"));
    }

    #[test]
    fn the_launcher_mount_wins_over_installed_bundles() {
        let base = tempfile::tempdir().unwrap();
        let mounted = install(&base.path().join("mount"));
        install(&base.path().join("user"));
        install(&base.path().join("system"));
        assert_eq!(roots(base.path()).locate().unwrap(), mounted);
    }

    #[test]
    fn a_user_bundle_wins_over_the_image_bundle() {
        let base = tempfile::tempdir().unwrap();
        let user = install(&base.path().join("user"));
        install(&base.path().join("system"));
        assert_eq!(roots(base.path()).locate().unwrap(), user);
    }

    #[test]
    fn the_image_bundle_is_found_outside_the_launcher() {
        let base = tempfile::tempdir().unwrap();
        let system = install(&base.path().join("system"));
        assert_eq!(roots(base.path()).locate().unwrap(), system);
    }

    #[test]
    fn a_developer_host_wins_when_named() {
        let base = tempfile::tempdir().unwrap();
        install(&base.path().join("system"));
        let developer = base.path().join("target/chromium/tilo-chromium-host");
        std::fs::create_dir_all(developer.parent().unwrap()).unwrap();
        std::fs::write(&developer, b"host").unwrap();
        let mut roots = roots(base.path());
        roots.host_override = Some(developer.clone());
        assert_eq!(roots.locate().unwrap(), developer);
    }

    #[test]
    fn a_host_beside_the_executable_is_not_an_installed_engine() {
        let base = tempfile::tempdir().unwrap();
        for dir in ["app/bin/chromium", "app/chromium", "app/Frameworks"] {
            let host = base.path().join(dir).join("tilo-chromium-host");
            std::fs::create_dir_all(host.parent().unwrap()).unwrap();
            std::fs::write(host, b"host").unwrap();
        }
        assert!(roots(base.path()).locate().is_err());
    }

    #[test]
    fn a_bundle_directory_without_the_host_is_not_installed() {
        let base = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(base.path().join("system").join(RUNTIME_VERSION).join("lib"))
            .unwrap();
        assert!(roots(base.path()).locate().is_err());
    }
}
