//! Installed qOS app registry lookup + launch surface.
//!
//! The compositor spawns three flavours of tab today:
//!
//!   * Baked-in system apps (`Terminal`, `FileExplorer`, `Notes`,
//!     `Editor`) — code-owned widgets constructed inline in
//!     [`crate::shell::mod`].
//!   * `.desktop` legacy apps — spawned via `bwrap` through
//!     [`crate::sandbox::spawn::spawn_in_workspace`].
//!   * **Installed qOS packages** (`.qpkg`) — handled here. The
//!     compositor calls [`spawn_installed_app`], which invokes
//!     `qos-install run <pkg-id>` as a subprocess; that in turn
//!     execs `qos-launcher` with the resolved manifest + payload
//!     root. The subprocess handles bwrap + landlock + seccomp;
//!     the compositor just needs the child PID.
//!
//! The registry file is written by `qos-install install` at
//! `$XDG_DATA_HOME/qos-apps/registry.toml`. This module only
//! *reads* it — installation is out-of-band, driven by the CLI or
//! the future app store.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};

use serde::Deserialize;

/// Which entrypoint flavour a package exposes. Matches the launcher
/// manifest's `entrypoint.type` field verbatim so a categoriser
/// (e.g. app tray vs command palette) can filter without loading
/// the manifest.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum EntrypointKind {
    #[default]
    Gui,
    Cli,
    Both,
}

impl EntrypointKind {
    fn from_wire(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "cli" => EntrypointKind::Cli,
            "both" => EntrypointKind::Both,
            _ => EntrypointKind::Gui,
        }
    }
}

/// One installed app as surfaced to the compositor.
///
/// Kept intentionally thin — a future app-store UI can extend this
/// with icon bytes, screenshots, changelog etc. For the spawn path
/// today we need `id` + the payload/manifest paths, and for the
/// (upcoming) app tray we need `name` + `categories`.
#[derive(Debug, Clone)]
pub struct InstalledApp {
    /// Reverse-DNS package id, e.g. `dev.qos.hello`.
    pub id: String,
    /// Human-readable name — falls back to `id` when the manifest
    /// omits `[package].name`.
    pub name: String,
    /// Semver — copied from the manifest at install time.
    pub version: String,
    /// Icon reference (path or freedesktop icon name). Empty when
    /// the manifest omits it.
    pub icon: String,
    /// Freedesktop category strings, e.g. `["Utility"]`. Empty when
    /// unspecified.
    pub categories: Vec<String>,
    /// GUI vs CLI vs both — controls which launcher path is used.
    pub entrypoint: EntrypointKind,
    /// Absolute path to the unpacked payload dir.
    pub payload_root: PathBuf,
    /// Absolute path to the extracted manifest.toml.
    pub manifest_path: PathBuf,
}

/// Snapshot of `$XDG_DATA_HOME/qos-apps/registry.toml`.
///
/// Load once at compositor start (cheap — a few dozen entries at
/// most) and re-load on `SIGHUP` or when the app store finishes an
/// install. `List` is the only useful operation.
#[derive(Debug, Clone, Default)]
pub struct InstalledAppRegistry {
    entries: Vec<InstalledApp>,
    source_path: Option<PathBuf>,
}

impl InstalledAppRegistry {
    /// Empty registry — used when no `registry.toml` exists yet.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Load from the default location — `$QOS_APPS_DATA_DIR` when
    /// set (dev / tests), else `$XDG_DATA_HOME/qos-apps` else
    /// `$HOME/.local/share/qos-apps`.
    pub fn load() -> Result<Self, RegistryError> {
        let path = default_registry_path()?;
        Self::load_from(&path)
    }

    /// Load from an explicit path — used by tests. `Ok(empty)` when
    /// the file does not exist (a fresh install has no registry).
    pub fn load_from(path: &Path) -> Result<Self, RegistryError> {
        if !path.exists() {
            return Ok(Self {
                entries: Vec::new(),
                source_path: Some(path.to_path_buf()),
            });
        }
        let text = fs::read_to_string(path).map_err(|source| RegistryError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        let raw: RawRegistry =
            toml::from_str(&text).map_err(|e| RegistryError::Parse(e.to_string()))?;
        let entries: Vec<InstalledApp> = raw.entry.into_iter().map(Into::into).collect();
        Ok(Self {
            entries,
            source_path: Some(path.to_path_buf()),
        })
    }

    /// All installed packages, in the on-disk order (as written by
    /// `qos-install install`, i.e. install order).
    pub fn entries(&self) -> &[InstalledApp] {
        &self.entries
    }

    /// Locate a package by reverse-DNS id.
    pub fn find(&self, pkg_id: &str) -> Option<&InstalledApp> {
        self.entries.iter().find(|e| e.id == pkg_id)
    }

    /// Path this registry was loaded from — informational, useful
    /// for debug overlays. `None` for [`Self::empty`].
    pub fn source_path(&self) -> Option<&Path> {
        self.source_path.as_deref()
    }
}

/// TOML wire shape — a subset of the fields `qos-install install`
/// writes. Extra fields are ignored so the wire remains
/// forward-compatible.
#[derive(Debug, Deserialize)]
struct RawRegistry {
    #[serde(default)]
    entry: Vec<RawEntry>,
}

#[derive(Debug, Deserialize)]
struct RawEntry {
    id: String,
    #[serde(default)]
    version: String,
    payload_root: PathBuf,
    manifest_path: PathBuf,
    /// Human name — extra, not written by every version of the CLI.
    #[serde(default)]
    name: String,
    #[serde(default)]
    icon: String,
    #[serde(default)]
    categories: Vec<String>,
    /// `gui` / `cli` / `both` — mirrors the manifest.
    #[serde(default, rename = "entrypoint_type")]
    entrypoint_type: Option<String>,
}

impl From<RawEntry> for InstalledApp {
    fn from(r: RawEntry) -> Self {
        let entrypoint = r
            .entrypoint_type
            .as_deref()
            .map(EntrypointKind::from_wire)
            .unwrap_or_default();
        let name = if r.name.is_empty() {
            r.id.clone()
        } else {
            r.name
        };
        InstalledApp {
            id: r.id,
            name,
            version: r.version,
            icon: r.icon,
            categories: r.categories,
            entrypoint,
            payload_root: r.payload_root,
            manifest_path: r.manifest_path,
        }
    }
}

/// Errors reading the on-disk registry.
#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("HOME / XDG_DATA_HOME not set — cannot locate registry")]
    NoHome,
    #[error("reading registry at {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("registry parse: {0}")]
    Parse(String),
}

/// Default registry path. Honours `QOS_APPS_DATA_DIR` and
/// `XDG_DATA_HOME` in that order; falls back to
/// `$HOME/.local/share/qos-apps/registry.toml`. Must stay in lockstep
/// with `crates/packaging/install::data_root` — the two decode the
/// same file.
pub fn default_registry_path() -> Result<PathBuf, RegistryError> {
    Ok(default_data_dir()?.join("registry.toml"))
}

/// Root install directory. Same lookup rules as
/// [`default_registry_path`]. Public so callers that need to place
/// sibling files (e.g. an app-tray icon cache) share the base.
pub fn default_data_dir() -> Result<PathBuf, RegistryError> {
    if let Some(dir) = std::env::var_os("QOS_APPS_DATA_DIR") {
        return Ok(PathBuf::from(dir));
    }
    if let Some(xdg) = std::env::var_os("XDG_DATA_HOME") {
        if !xdg.is_empty() {
            return Ok(PathBuf::from(xdg).join("qos-apps"));
        }
    }
    let home = std::env::var_os("HOME").ok_or(RegistryError::NoHome)?;
    Ok(Path::new(&home).join(".local/share/qos-apps"))
}

/// Options passed to [`spawn_installed_app`].
#[derive(Debug, Clone, Default)]
pub struct SpawnOptions {
    /// If set, override the `qos-install` binary path (tests). When
    /// `None`, whatever is on `$PATH` is used — production installs
    /// have it at `/usr/bin/qos-install`.
    pub qos_install_bin: Option<PathBuf>,
    /// Extra env vars to hand to `qos-install run`. The typical
    /// caller sets `QOS_WORKSPACE_ID` here so the launcher's
    /// `sandbox.workspace_grants = "auto"` reader knows which
    /// workspace file to consult.
    pub env: Vec<(String, String)>,
    /// Skip service resolution — pass-through of `qos-install run
    /// --no-services`, mainly useful for a hermetic CLI probe.
    pub no_services: bool,
}

/// Launch an installed package via `qos-install run <pkg-id>`.
///
/// The child process handles bwrap + Landlock + seccomp itself
/// (it delegates to `qos-launcher run`). The compositor only needs
/// the [`Child`] handle so it can associate the resulting Wayland
/// surface with a tab.
///
/// On success the child is spawned in the background. Errors bubble
/// up as [`io::Error`] — the caller should surface them as a toast.
pub fn spawn_installed_app(pkg_id: &str, opts: &SpawnOptions) -> io::Result<Child> {
    let bin = opts
        .qos_install_bin
        .clone()
        .unwrap_or_else(|| PathBuf::from("qos-install"));
    let mut cmd = Command::new(bin);
    cmd.arg("run").arg(pkg_id);
    if opts.no_services {
        cmd.arg("--no-services");
    }
    for (k, v) in &opts.env {
        cmd.env(k, v);
    }
    cmd.spawn()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct EnvGuard {
        key: &'static str,
        prev: Option<std::ffi::OsString>,
    }
    impl EnvGuard {
        fn set(key: &'static str, val: &str) -> Self {
            let prev = std::env::var_os(key);
            // SAFETY: single-thread env mutation for test setup;
            // Drop restores the previous value.
            unsafe { std::env::set_var(key, val) };
            Self { key, prev }
        }
    }
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match self.prev.take() {
                Some(v) => unsafe { std::env::set_var(self.key, v) },
                None => unsafe { std::env::remove_var(self.key) },
            }
        }
    }

    #[test]
    fn empty_registry_when_file_missing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let missing = tmp.path().join("registry.toml");
        let reg = InstalledAppRegistry::load_from(&missing).expect("load");
        assert!(reg.entries().is_empty());
        assert!(reg.find("dev.qos.hello").is_none());
    }

    #[test]
    fn load_parses_registry_from_disk() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let payload = tmp.path().join("payloads/dev.qos.hello");
        std::fs::create_dir_all(&payload).unwrap();
        let manifest = payload.join("manifest.toml");
        std::fs::write(&manifest, "package_id='dev.qos.hello'\nversion='0.1.0'\n").unwrap();
        let registry = tmp.path().join("registry.toml");
        std::fs::write(
            &registry,
            format!(
                "[[entry]]\n\
                 id = 'dev.qos.hello'\n\
                 version = '0.1.0'\n\
                 payload_root = '{}'\n\
                 manifest_path = '{}'\n\
                 name = 'qos-hello'\n\
                 categories = ['Utility']\n\
                 entrypoint_type = 'gui'\n",
                payload.display(),
                manifest.display()
            ),
        )
        .unwrap();

        let reg = InstalledAppRegistry::load_from(&registry).expect("load");
        assert_eq!(reg.entries().len(), 1);
        let entry = reg.find("dev.qos.hello").expect("present");
        assert_eq!(entry.id, "dev.qos.hello");
        assert_eq!(entry.name, "qos-hello");
        assert_eq!(entry.version, "0.1.0");
        assert_eq!(entry.categories, vec!["Utility"]);
        assert_eq!(entry.entrypoint, EntrypointKind::Gui);
        assert_eq!(entry.payload_root, payload);
    }

    #[test]
    fn load_tolerates_registry_written_by_older_cli() {
        // Older versions of qos-install wrote only the four base
        // fields — the compositor must load these without erroring.
        let tmp = tempfile::tempdir().expect("tempdir");
        let registry = tmp.path().join("registry.toml");
        std::fs::write(
            &registry,
            "[[entry]]\n\
             id = 'dev.qos.older'\n\
             version = '0.1.0'\n\
             payload_root = '/tmp/older'\n\
             manifest_path = '/tmp/older/manifest.toml'\n",
        )
        .unwrap();

        let reg = InstalledAppRegistry::load_from(&registry).expect("load");
        let entry = reg.find("dev.qos.older").expect("present");
        // Defaults kick in for the missing fields.
        assert_eq!(entry.name, "dev.qos.older");
        assert_eq!(entry.entrypoint, EntrypointKind::Gui);
        assert!(entry.categories.is_empty());
    }

    #[test]
    fn default_data_dir_honours_qos_override() {
        let _g = EnvGuard::set("QOS_APPS_DATA_DIR", "/tmp/override");
        let dir = default_data_dir().expect("dir");
        assert_eq!(dir, PathBuf::from("/tmp/override"));
    }

    #[test]
    fn default_data_dir_falls_back_to_xdg() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let _clear = EnvGuard::set("QOS_APPS_DATA_DIR", "");
        // Clearing an env var via set-then-take is racy across
        // threads — this test lives alongside similar racy
        // env-touching tests already in the codebase.
        unsafe { std::env::remove_var("QOS_APPS_DATA_DIR") };
        let _g = EnvGuard::set("XDG_DATA_HOME", tmp.path().to_str().unwrap());
        let dir = default_data_dir().expect("dir");
        assert_eq!(dir, tmp.path().join("qos-apps"));
    }

    #[test]
    fn spawn_options_default_are_reasonable() {
        let opts = SpawnOptions::default();
        assert!(opts.qos_install_bin.is_none());
        assert!(opts.env.is_empty());
        assert!(!opts.no_services);
    }
}
