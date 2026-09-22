//! Minimal read-side of the tilo-install registry.
//!
//! `tilo-install` writes the canonical `registry.toml` under
//! `$XDG_DATA_HOME/tilo-apps/registry.toml`. The launcher only needs to
//! look packages up by id, so it decodes a strict subset of the shape
//! here rather than pulling in the whole `install` crate.
//!
//! There are **two** such registries, read the same way: the system
//! store at [`SYSTEM_DATA_DIR`] (written at image-build time, ships
//! read-only inside the image so a fresh boot has a populated
//! launcher), and the per-user store. Both describe packages the same
//! way — being preinstalled is a matter of *where the payload lives*,
//! not what the package is allowed to do, so the launcher grants a
//! system entry exactly what it grants a user entry. A user entry
//! shadows a system entry with the same id.
//!
//! Kept intentionally thin — a future `app-host` socket
//! (`docs/app-host.md`) will replace this file-based lookup.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// One installed-package row — only the fields `run-installed` needs today.
#[derive(Debug, Clone, Deserialize)]
pub struct InstalledPackage {
    /// Reverse-DNS package id.
    pub id: String,
    /// Content-addressed signed package root recorded by `tilo-install`.
    /// Empty only for legacy/dev fixtures; security-sensitive callers must
    /// reject it rather than deriving trust from the mutable payload path.
    #[serde(default)]
    pub package_id_hex: String,
    /// Path to the unpacked payload root.
    pub payload_root: PathBuf,
    /// Path to the extracted manifest.
    pub manifest_path: PathBuf,
}

/// Load-and-search wrapper: parses `registry.toml`, returns the entry
/// matching `pkg_id`, or `None` if not installed.
///
/// # Errors
///
/// Returns [`RegistryError`] if either registry exists but fails to read or
/// parse, or if neither `HOME` nor `XDG_DATA_HOME` is set.
pub fn find_installed(pkg_id: &str) -> Result<Option<InstalledPackage>, RegistryError> {
    Ok(list_installed()?.into_iter().find(|e| e.id == pkg_id))
}

/// Every package either registry knows about: the system store first,
/// then the per-user one, with a user entry shadowing a system entry of
/// the same id.
///
/// A missing `registry.toml` is "nothing installed", not an error. A
/// registry that exists but doesn't parse *is* an error — silently
/// reporting "no apps" would hide the breakage.
///
/// # Errors
///
/// Returns [`RegistryError`] if either registry exists but fails to read or
/// parse, or if neither `HOME` nor `XDG_DATA_HOME` is set.
pub fn list_installed() -> Result<Vec<InstalledPackage>, RegistryError> {
    let mut out = match system_registry_path() {
        Some(path) => read_registry(&path)?,
        None => Vec::new(),
    };
    for entry in read_registry(&default_registry_path()?)? {
        match out.iter_mut().find(|e| e.id == entry.id) {
            // Same id in both stores: the user's install runs. Replacing in place
            // keeps the system store's ordering stable.
            Some(slot) => *slot = entry,
            None => out.push(entry),
        }
    }
    Ok(out)
}

/// Parse one `registry.toml`. A missing path yields no entries;
/// anything else that goes wrong is reported.
fn read_registry(path: &Path) -> Result<Vec<InstalledPackage>, RegistryError> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let raw = fs::read_to_string(path).map_err(|e| RegistryError::Read {
        path: path.to_path_buf(),
        source: e,
    })?;
    let reg: Registry = toml::from_str(&raw).map_err(|e| RegistryError::Parse(e.to_string()))?;
    Ok(reg.entry)
}

#[derive(Debug, Deserialize)]
struct Registry {
    #[serde(default)]
    entry: Vec<InstalledPackage>,
}

/// `so.tilo.text-editor` → `Text Editor`.
///
/// The fallback display name for a package with none — the trailing id
/// component is the only signal there is. Separators become spaces and
/// each word is title-cased.
///
/// Lives here rather than in either caller because the desktop derives
/// it for an installed package with no manifest name, and the
/// catalogue for an available one with no registry name — the same
/// package must not be called two different things.
pub fn display_name_for_package_id(package_id: &str) -> String {
    let last = package_id
        .rsplit('.')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(package_id);
    let words: Vec<String> = last
        .split(['-', '_'])
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut chars = w.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect();
    if words.is_empty() {
        package_id.to_string()
    } else {
        words.join(" ")
    }
}

/// Resolve `~/.local/share/tilo-apps/registry.toml`. Kept separate so
/// tests can point the launcher at a temp dir via `TILO_APPS_DATA_DIR`.
///
/// # Errors
///
/// Returns [`RegistryError::NoHome`] if neither `HOME` nor
/// `XDG_DATA_HOME`/`TILO_APPS_DATA_DIR` is set.
pub fn default_registry_path() -> Result<PathBuf, RegistryError> {
    Ok(default_data_dir()?.join("registry.toml"))
}

/// The image-wide app store. Read-only at runtime: populated during
/// the image build so a first boot already has a registry to list.
pub const SYSTEM_DATA_DIR: &str = "/usr/share/tilo-apps";

/// Whether this session was handed an app store of its own.
///
/// True exactly when `TILO_APPS_DATA_DIR` names the store and no
/// `TILO_APPS_SYSTEM_DIR` puts the machine's back beside it. The answer
/// means "which applications this session has is settled, and this
/// directory is the whole of it". A production desktop sets neither
/// and gets `false`.
pub fn store_is_scoped_to_this_session() -> bool {
    std::env::var_os("TILO_APPS_SYSTEM_DIR").is_none()
        && std::env::var_os("TILO_APPS_DATA_DIR").is_some()
}

/// Root of the system store, or `None` when there is no system store
/// to read.
///
/// `TILO_APPS_SYSTEM_DIR` names it explicitly; otherwise it's
/// [`SYSTEM_DATA_DIR`] — *unless* `TILO_APPS_DATA_DIR` is set, which
/// suppresses it entirely. That's what makes the override usable for
/// tests: `TILO_APPS_DATA_DIR` means "the store is here", and a test
/// asserting on package count needs that to be the whole answer, or
/// the same test would pass on a dev box and fail inside a Tilo image
/// with real preinstalled packages counted alongside fixtures.
pub fn system_data_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("TILO_APPS_SYSTEM_DIR") {
        return Some(PathBuf::from(dir));
    }
    if store_is_scoped_to_this_session() {
        return None;
    }
    Some(PathBuf::from(SYSTEM_DATA_DIR))
}

/// `registry.toml` of the system store, when there is one. Needs no
/// `HOME`, unlike the per-user path — resolves the same for every user.
pub fn system_registry_path() -> Option<PathBuf> {
    Some(system_data_dir()?.join("registry.toml"))
}

/// Root install directory. Honours the `TILO_APPS_DATA_DIR` override for
/// tests/dev-mode; falls back to `$XDG_DATA_HOME` or
/// `$HOME/.local/share/tilo-apps`.
///
/// # Errors
///
/// Returns [`RegistryError::NoHome`] if neither `HOME` nor
/// `XDG_DATA_HOME`/`TILO_APPS_DATA_DIR` is set.
pub fn default_data_dir() -> Result<PathBuf, RegistryError> {
    if let Some(override_dir) = std::env::var_os("TILO_APPS_DATA_DIR") {
        return Ok(PathBuf::from(override_dir));
    }
    if let Some(xdg) = std::env::var_os("XDG_DATA_HOME") {
        return Ok(PathBuf::from(xdg).join("tilo-apps"));
    }
    let home = std::env::var_os("HOME").ok_or(RegistryError::NoHome)?;
    Ok(Path::new(&home).join(".local/share/tilo-apps"))
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

    // Every test here sets `TILO_APPS_DATA_DIR` (process-wide), so they run
    // in the `process_env` serial group, and pin `TILO_APPS_SYSTEM_DIR` at
    // an empty dir so the reader doesn't fall back to the real
    // `/usr/share/tilo-apps` and see the whole preinstalled fleet.

    /// Write a `registry.toml` naming `ids`, with a payload dir and
    /// manifest on disk for each, and return the directory.
    fn store_with(ids: &[&str]) -> tempfile::TempDir {
        use std::fmt::Write as _;
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut toml_src = String::new();
        for id in ids {
            let payload = tmp.path().join("payloads").join(id);
            std::fs::create_dir_all(&payload).unwrap();
            let manifest = payload.join("manifest.toml");
            std::fs::write(&manifest, "").unwrap();
            let _ = write!(
                toml_src,
                "[[entry]]\nid = '{id}'\npayload_root = {:?}\nmanifest_path = {:?}\n",
                payload.display().to_string(),
                manifest.display().to_string()
            );
        }
        std::fs::write(tmp.path().join("registry.toml"), toml_src).unwrap();
        tmp
    }

    /// Run `f` with both store roots pointed at the given directories.
    fn with_stores<T>(system: &Path, user: &Path, f: impl FnOnce() -> T) -> T {
        // SAFETY: callers are all in the `process_env` serial group.
        unsafe {
            std::env::set_var("TILO_APPS_SYSTEM_DIR", system);
            std::env::set_var("TILO_APPS_DATA_DIR", user);
        }
        let out = f();
        unsafe {
            std::env::remove_var("TILO_APPS_SYSTEM_DIR");
            std::env::remove_var("TILO_APPS_DATA_DIR");
        }
        out
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn system_store_entries_are_listed_without_any_user_install() {
        // The preinstalled case: an image ships a system registry and the user
        // never ran `tilo-install`. The palette must still have something to list.
        let system = store_with(&["so.tilo.notes", "so.tilo.calculator"]);
        let user = tempfile::tempdir().expect("tempdir");
        let all = with_stores(system.path(), user.path(), list_installed).expect("read");
        let ids: Vec<&str> = all.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, vec!["so.tilo.notes", "so.tilo.calculator"]);
        // And a lookup by id resolves to the system payload.
        let found = with_stores(system.path(), user.path(), || {
            find_installed("so.tilo.notes")
        })
        .expect("read")
        .expect("preinstalled notes is installed");
        assert!(found.payload_root.starts_with(system.path()));
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn both_stores_merge_and_a_user_install_shadows_the_system_one() {
        let system = store_with(&["so.tilo.notes", "so.tilo.calculator"]);
        let user = store_with(&["so.tilo.notes", "so.tilo.mine"]);
        let all = with_stores(system.path(), user.path(), list_installed).expect("read");
        let ids: Vec<&str> = all.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["so.tilo.notes", "so.tilo.calculator", "so.tilo.mine"],
            "one row per id: the shadowed system entry must not appear twice",
        );
        let notes = all.iter().find(|e| e.id == "so.tilo.notes").unwrap();
        assert!(
            notes.payload_root.starts_with(user.path()),
            "a user install of a preinstalled id is the one that runs, got {:?}",
            notes.payload_root,
        );
        // The system-only entry is untouched by the shadowing.
        let calc = all.iter().find(|e| e.id == "so.tilo.calculator").unwrap();
        assert!(calc.payload_root.starts_with(system.path()));
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn tilo_apps_data_dir_alone_suppresses_the_system_store() {
        // The isolation rule the shell's fixture tests depend on: setting only
        // TILO_APPS_DATA_DIR means "the store is exactly here" — otherwise a
        // test asserting a package count would pass on a dev box and fail
        // inside a Tilo image where /usr/share/tilo-apps is populated.
        let user = store_with(&["so.tilo.mine"]);
        // SAFETY: this test is in the `process_env` serial group.
        unsafe { std::env::set_var("TILO_APPS_DATA_DIR", user.path()) };
        let resolved = system_data_dir();
        let all = list_installed().expect("read");
        unsafe { std::env::remove_var("TILO_APPS_DATA_DIR") };
        assert!(
            resolved.is_none(),
            "TILO_APPS_DATA_DIR without TILO_APPS_SYSTEM_DIR must suppress the \
             system store, got {resolved:?}",
        );
        let ids: Vec<&str> = all.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, vec!["so.tilo.mine"]);

        // With neither set, the real system path is what gets read.
        assert_eq!(
            system_data_dir(),
            Some(PathBuf::from(SYSTEM_DATA_DIR)),
            "production reads the image's store",
        );
    }

    /// The same rule asked as a question, for callers that want to know
    /// whether this session's applications are settled.
    #[test]
    #[serial_test::serial(process_env)]
    fn a_store_is_scoped_only_when_it_is_the_whole_answer() {
        // SAFETY: this test is in the `process_env` serial group.
        unsafe {
            std::env::remove_var("TILO_APPS_DATA_DIR");
            std::env::remove_var("TILO_APPS_SYSTEM_DIR");
        }
        assert!(
            !store_is_scoped_to_this_session(),
            "a production desktop sets neither and reads both stores",
        );

        unsafe { std::env::set_var("TILO_APPS_DATA_DIR", "/tmp/tilo-apps-fixture") };
        assert!(
            store_is_scoped_to_this_session(),
            "TILO_APPS_DATA_DIR alone is what `the store is exactly here` is \
             spelled as",
        );

        unsafe { std::env::set_var("TILO_APPS_SYSTEM_DIR", "/tmp/tilo-apps-system") };
        assert!(
            !store_is_scoped_to_this_session(),
            "naming the system store too asks for both, which is a machine-wide \
             reading and not a scoped session",
        );

        unsafe {
            std::env::remove_var("TILO_APPS_DATA_DIR");
            std::env::remove_var("TILO_APPS_SYSTEM_DIR");
        }
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn a_missing_system_store_is_not_an_error() {
        // A plain dev machine has no /usr/share/tilo-apps — that reads as
        // "nothing preinstalled", not a failure hiding the user's own installs.
        let system = tempfile::tempdir().expect("tempdir");
        let user = store_with(&["so.tilo.mine"]);
        let all = with_stores(system.path(), user.path(), list_installed).expect("read");
        let ids: Vec<&str> = all.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, vec!["so.tilo.mine"]);
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn find_installed_reads_registry_via_override_env() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let payload = tmp.path().join("payloads/so.tilo.notes-cli");
        std::fs::create_dir_all(&payload).unwrap();
        let manifest = payload.join("manifest.toml");
        std::fs::write(
            &manifest,
            "package_id = 'so.tilo.notes-cli'\nversion='0.1.0'\n[entrypoint]\nbin='bin/notes'\n",
        )
        .unwrap();
        let registry_toml = format!(
            "[[entry]]\nid = 'so.tilo.notes-cli'\npayload_root = {:?}\nmanifest_path = {:?}\n",
            payload.display().to_string(),
            manifest.display().to_string()
        );
        std::fs::write(tmp.path().join("registry.toml"), registry_toml).unwrap();

        let empty_system = tempfile::tempdir().expect("tempdir");
        let found = with_stores(empty_system.path(), tmp.path(), || {
            find_installed("so.tilo.notes-cli")
        })
        .expect("read");
        let entry = found.expect("so.tilo.notes-cli present in registry");
        assert_eq!(entry.id, "so.tilo.notes-cli");
        assert_eq!(entry.payload_root, payload);
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn list_installed_returns_every_entry_in_file_order() {
        use std::fmt::Write as _;
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut toml_src = String::new();
        for id in ["so.tilo.alpha", "so.tilo.beta"] {
            let payload = tmp.path().join("payloads").join(id);
            std::fs::create_dir_all(&payload).unwrap();
            let manifest = payload.join("manifest.toml");
            std::fs::write(&manifest, "").unwrap();
            let _ = write!(
                toml_src,
                "[[entry]]\nid = '{id}'\npayload_root = {:?}\nmanifest_path = {:?}\n",
                payload.display().to_string(),
                manifest.display().to_string()
            );
        }
        std::fs::write(tmp.path().join("registry.toml"), toml_src).unwrap();

        // SAFETY: this test is in the `process_env` serial group.
        unsafe { std::env::set_var("TILO_APPS_DATA_DIR", tmp.path()) };
        let all = list_installed().expect("read");
        unsafe { std::env::remove_var("TILO_APPS_DATA_DIR") };
        let ids: Vec<&str> = all.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, vec!["so.tilo.alpha", "so.tilo.beta"]);
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn list_installed_is_empty_when_registry_missing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        unsafe { std::env::set_var("TILO_APPS_DATA_DIR", tmp.path()) };
        let all = list_installed().expect("read");
        unsafe { std::env::remove_var("TILO_APPS_DATA_DIR") };
        assert!(all.is_empty());
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn find_installed_returns_none_when_registry_missing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // No registry.toml written.
        unsafe { std::env::set_var("TILO_APPS_DATA_DIR", tmp.path()) };
        let found = find_installed("nothing").expect("read");
        unsafe { std::env::remove_var("TILO_APPS_DATA_DIR") };
        assert!(found.is_none());
    }
}
