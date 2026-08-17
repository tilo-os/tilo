//! Minimal read-side of the qos-install registry.
//!
//! `qos-install` writes the canonical `registry.toml` under
//! `$XDG_DATA_HOME/qos-apps/registry.toml` (or `~/.local/share/qos-apps/`).
//! The launcher only needs to look packages up by id, so we
//! decode a strict subset of the shape here rather than pulling
//! in the whole `install` crate.
//!
//! There are **two** such registries, read in the same shape:
//!
//! * the system store at [`SYSTEM_DATA_DIR`], written at image-build
//!   time by running the ordinary `qos-install install` against a
//!   `QOS_APPS_DATA_DIR` pointed there. It ships read-only inside the
//!   image, which is what gives a fresh boot a populated launcher
//!   without the user having to install anything.
//! * the per-user store, unchanged.
//!
//! Both are produced by the same installer and describe packages the
//! same way; being preinstalled is a matter of *where the payload
//! lives*, not of what the package is allowed to do. The launcher
//! grants a system entry exactly what it grants a user entry — the
//! sandbox is built from the manifest either way, the payload is
//! ro-bound either way, and the one writable directory comes from
//! [`crate::appdir`] either way.
//!
//! A user entry shadows a system entry with the same id, so installing
//! your own build of a preinstalled app replaces it rather than
//! producing a duplicate row in the palette.
//!
//! Where those two stores are and in which order they are read is not
//! decided here — it lives in [`pkg::store`], because `qos-install`
//! reads the same two stores and the two readers disagreeing about what
//! is installed is precisely the bug that module exists to prevent. This
//! file keeps only the row shape the launcher needs.
//!
//! Kept intentionally thin — a future `app-host` socket
//! (`docs/app-host.md`) will replace this file-based lookup.
//! When it lands, the `run-installed` subcommand should try the
//! socket first and fall back to this reader.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// One installed-package row. Only the fields the launcher's
/// `run-installed` subcommand needs today.
#[derive(Debug, Clone, Deserialize)]
pub struct InstalledPackage {
    /// Reverse-DNS package id.
    pub id: String,
    /// Path to the unpacked payload root.
    pub payload_root: PathBuf,
    /// Path to the extracted manifest.
    pub manifest_path: PathBuf,
}

/// Load-and-search wrapper: parses `registry.toml`, returns the
/// entry matching `pkg_id`, or `None` if it isn't installed.
pub fn find_installed(pkg_id: &str) -> Result<Option<InstalledPackage>, RegistryError> {
    Ok(list_installed()?.into_iter().find(|e| e.id == pkg_id))
}

/// Every package either registry knows about: the system store first,
/// then the per-user one, with a user entry shadowing a system entry of
/// the same id.
///
/// A missing `registry.toml` is "nothing installed", not an error — a
/// machine with no preinstalled set has no system registry, and a fresh
/// user has no user registry until the first `qos-install install`.
/// A registry that exists but doesn't parse *is* an error: silently
/// reporting "no apps" for a corrupt file would hide the breakage from
/// whoever is looking at an empty launcher.
pub fn list_installed() -> Result<Vec<InstalledPackage>, RegistryError> {
    Ok(pkg::store::merge_registries(read_registry, |e| e.id.as_str())?
        .into_iter()
        .map(|(_, entry)| entry)
        .collect())
}

/// Parse one `registry.toml`. A path that does not exist yields no
/// entries; anything else that goes wrong is reported.
fn read_registry(path: &Path) -> Result<Vec<InstalledPackage>, RegistryError> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let raw = fs::read_to_string(path).map_err(|e| RegistryError::Read {
        path: path.to_path_buf(),
        source: e,
    })?;
    let reg: Registry = toml::from_str(&raw)
        .map_err(|e| RegistryError::Parse(e.to_string()))?;
    Ok(reg.entry)
}

#[derive(Debug, Deserialize)]
struct Registry {
    #[serde(default)]
    entry: Vec<InstalledPackage>,
}

/// Resolve `~/.local/share/qos-apps/registry.toml`. Kept
/// separate so tests can point the launcher at a temporary
/// dir via `QOS_APPS_DATA_DIR`.
pub fn default_registry_path() -> Result<PathBuf, RegistryError> {
    Ok(default_data_dir()?.join("registry.toml"))
}

/// The image-wide app store. Read-only at runtime: it is populated
/// during the image build and ships inside the immutable rootfs, so a
/// first boot already has a registry to list.
pub use pkg::store::SYSTEM_DATA_DIR;

/// Root of the system store, or `None` when there is no system store to
/// read. See [`pkg::store::system_data_dir`] for the override rules.
pub use pkg::store::system_data_dir;

/// `registry.toml` of the system store, when there is one. Unlike the
/// per-user path this needs no `HOME` — it resolves identically for
/// every user and for a launch with no home at all.
pub use pkg::store::system_registry_path;

/// Root install directory. Honours the `QOS_APPS_DATA_DIR`
/// override so integration tests (and dev-mode runs) can point
/// the launcher at a temp dir. Falls back to `$XDG_DATA_HOME`
/// or `$HOME/.local/share/qos-apps`.
pub fn default_data_dir() -> Result<PathBuf, RegistryError> {
    pkg::store::user_data_dir().ok_or(RegistryError::NoHome)
}

#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("HOME / XDG_DATA_HOME not set — cannot locate registry")]
    NoHome,
    #[error("reading registry at {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("registry parse: {0}")]
    Parse(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    // Every test here sets `QOS_APPS_DATA_DIR`, which is process-wide:
    // run in parallel they clobber each other's override (and the last
    // one to finish removes the var out from under the others). Same
    // `process_env` serial group as the tests in `run` that shim `HOME`.
    //
    // They also pin `QOS_APPS_SYSTEM_DIR` at an empty directory. Without
    // it the reader would fall back to the real `/usr/share/qos-apps`,
    // so the same test would pass on a dev box and see the whole
    // preinstalled fleet on an actual qOS install.

    /// Write a `registry.toml` naming `ids`, with a payload dir and
    /// manifest on disk for each, and return the directory.
    fn store_with(ids: &[&str]) -> tempfile::TempDir {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut toml_src = String::new();
        for id in ids {
            let payload = tmp.path().join("payloads").join(id);
            std::fs::create_dir_all(&payload).unwrap();
            let manifest = payload.join("manifest.toml");
            std::fs::write(&manifest, "").unwrap();
            toml_src.push_str(&format!(
                "[[entry]]\nid = '{id}'\npayload_root = {:?}\nmanifest_path = {:?}\n",
                payload.display().to_string(),
                manifest.display().to_string()
            ));
        }
        std::fs::write(tmp.path().join("registry.toml"), toml_src).unwrap();
        tmp
    }

    /// Run `f` with both store roots pointed at the given directories.
    fn with_stores<T>(system: &Path, user: &Path, f: impl FnOnce() -> T) -> T {
        // SAFETY: callers are all in the `process_env` serial group.
        unsafe {
            std::env::set_var("QOS_APPS_SYSTEM_DIR", system);
            std::env::set_var("QOS_APPS_DATA_DIR", user);
        }
        let out = f();
        unsafe {
            std::env::remove_var("QOS_APPS_SYSTEM_DIR");
            std::env::remove_var("QOS_APPS_DATA_DIR");
        }
        out
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn system_store_entries_are_listed_without_any_user_install() {
        // The preinstalled case: an image ships a system registry and the
        // user has never run `qos-install`. The palette must still have
        // something to list.
        let system = store_with(&["dev.qos.notes", "dev.qos.calculator"]);
        let user = tempfile::tempdir().expect("tempdir");
        let all = with_stores(system.path(), user.path(), || list_installed()).expect("read");
        let ids: Vec<&str> = all.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, vec!["dev.qos.notes", "dev.qos.calculator"]);
        // And a lookup by id resolves to the system payload.
        let found = with_stores(system.path(), user.path(), || {
            find_installed("dev.qos.notes")
        })
        .expect("read")
        .expect("preinstalled notes is installed");
        assert!(found.payload_root.starts_with(system.path()));
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn both_stores_merge_and_a_user_install_shadows_the_system_one() {
        let system = store_with(&["dev.qos.notes", "dev.qos.calculator"]);
        let user = store_with(&["dev.qos.notes", "dev.qos.mine"]);
        let all = with_stores(system.path(), user.path(), || list_installed()).expect("read");
        let ids: Vec<&str> = all.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["dev.qos.notes", "dev.qos.calculator", "dev.qos.mine"],
            "one row per id: the shadowed system entry must not appear twice",
        );
        let notes = all.iter().find(|e| e.id == "dev.qos.notes").unwrap();
        assert!(
            notes.payload_root.starts_with(user.path()),
            "a user install of a preinstalled id is the one that runs, got {:?}",
            notes.payload_root,
        );
        // The system-only entry is untouched by the shadowing.
        let calc = all.iter().find(|e| e.id == "dev.qos.calculator").unwrap();
        assert!(calc.payload_root.starts_with(system.path()));
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn qos_apps_data_dir_alone_suppresses_the_system_store() {
        // The isolation rule the shell's fixture tests depend on: setting
        // only QOS_APPS_DATA_DIR means "the store is exactly here". Without
        // this, every test that points at a temp dir and asserts a package
        // count would pass on a dev box and fail inside a qOS image, where
        // /usr/share/qos-apps is populated.
        let user = store_with(&["dev.qos.mine"]);
        // SAFETY: this test is in the `process_env` serial group.
        unsafe { std::env::set_var("QOS_APPS_DATA_DIR", user.path()) };
        let resolved = system_data_dir();
        let all = list_installed().expect("read");
        unsafe { std::env::remove_var("QOS_APPS_DATA_DIR") };
        assert!(
            resolved.is_none(),
            "QOS_APPS_DATA_DIR without QOS_APPS_SYSTEM_DIR must suppress the \
             system store, got {resolved:?}",
        );
        let ids: Vec<&str> = all.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, vec!["dev.qos.mine"]);

        // With neither set, the real system path is what gets read.
        assert_eq!(
            system_data_dir(),
            Some(PathBuf::from(SYSTEM_DATA_DIR)),
            "production reads the image's store",
        );
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn a_missing_system_store_is_not_an_error() {
        // A plain dev machine has no /usr/share/qos-apps. That has to
        // read as "nothing preinstalled", not as a failure that hides
        // the user's own installs.
        let system = tempfile::tempdir().expect("tempdir");
        let user = store_with(&["dev.qos.mine"]);
        let all = with_stores(system.path(), user.path(), || list_installed()).expect("read");
        let ids: Vec<&str> = all.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, vec!["dev.qos.mine"]);
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn find_installed_reads_registry_via_override_env() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let payload = tmp.path().join("payloads/dev.qos.notes-cli");
        std::fs::create_dir_all(&payload).unwrap();
        let manifest = payload.join("manifest.toml");
        std::fs::write(&manifest, "package_id = 'dev.qos.notes-cli'\nversion='0.1.0'\n[entrypoint]\nbin='bin/notes'\n").unwrap();
        let registry_toml = format!(
            "[[entry]]\nid = 'dev.qos.notes-cli'\npayload_root = {:?}\nmanifest_path = {:?}\n",
            payload.display().to_string(),
            manifest.display().to_string()
        );
        std::fs::write(tmp.path().join("registry.toml"), registry_toml).unwrap();

        let empty_system = tempfile::tempdir().expect("tempdir");
        let found = with_stores(empty_system.path(), tmp.path(), || {
            find_installed("dev.qos.notes-cli")
        })
        .expect("read");
        let entry = found.expect("dev.qos.notes-cli present in registry");
        assert_eq!(entry.id, "dev.qos.notes-cli");
        assert_eq!(entry.payload_root, payload);
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn list_installed_returns_every_entry_in_file_order() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut toml_src = String::new();
        for id in ["dev.qos.alpha", "dev.qos.beta"] {
            let payload = tmp.path().join("payloads").join(id);
            std::fs::create_dir_all(&payload).unwrap();
            let manifest = payload.join("manifest.toml");
            std::fs::write(&manifest, "").unwrap();
            toml_src.push_str(&format!(
                "[[entry]]\nid = '{id}'\npayload_root = {:?}\nmanifest_path = {:?}\n",
                payload.display().to_string(),
                manifest.display().to_string()
            ));
        }
        std::fs::write(tmp.path().join("registry.toml"), toml_src).unwrap();

        // SAFETY: this test is in the `process_env` serial group.
        unsafe { std::env::set_var("QOS_APPS_DATA_DIR", tmp.path()) };
        let all = list_installed().expect("read");
        unsafe { std::env::remove_var("QOS_APPS_DATA_DIR") };
        let ids: Vec<&str> = all.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, vec!["dev.qos.alpha", "dev.qos.beta"]);
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn list_installed_is_empty_when_registry_missing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        unsafe { std::env::set_var("QOS_APPS_DATA_DIR", tmp.path()) };
        let all = list_installed().expect("read");
        unsafe { std::env::remove_var("QOS_APPS_DATA_DIR") };
        assert!(all.is_empty());
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn find_installed_returns_none_when_registry_missing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // No registry.toml written.
        unsafe { std::env::set_var("QOS_APPS_DATA_DIR", tmp.path()) };
        let found = find_installed("nothing").expect("read");
        unsafe { std::env::remove_var("QOS_APPS_DATA_DIR") };
        assert!(found.is_none());
    }
}
