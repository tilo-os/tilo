//! Minimal read-side of the qos-install registry.
//!
//! `qos-install` writes the canonical `registry.toml` under
//! `$XDG_DATA_HOME/qos-apps/registry.toml` (or `~/.local/share/qos-apps/`).
//! The launcher only needs to look packages up by id, so we
//! decode a strict subset of the shape here rather than pulling
//! in the whole `install` crate.
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
    let path = default_registry_path()?;
    if !path.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(&path).map_err(|e| RegistryError::Read {
        path: path.clone(),
        source: e,
    })?;
    let reg: Registry = toml::from_str(&raw)
        .map_err(|e| RegistryError::Parse(e.to_string()))?;
    Ok(reg.entry.into_iter().find(|e| e.id == pkg_id))
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

/// Root install directory. Honours the `QOS_APPS_DATA_DIR`
/// override so integration tests (and dev-mode runs) can point
/// the launcher at a temp dir. Falls back to `$XDG_DATA_HOME`
/// or `$HOME/.local/share/qos-apps`.
pub fn default_data_dir() -> Result<PathBuf, RegistryError> {
    if let Some(override_dir) = std::env::var_os("QOS_APPS_DATA_DIR") {
        return Ok(PathBuf::from(override_dir));
    }
    if let Some(xdg) = std::env::var_os("XDG_DATA_HOME") {
        return Ok(PathBuf::from(xdg).join("qos-apps"));
    }
    let home = std::env::var_os("HOME").ok_or(RegistryError::NoHome)?;
    Ok(Path::new(&home).join(".local/share/qos-apps"))
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

    #[test]
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

        // SAFETY: no other thread reads QOS_APPS_DATA_DIR during this test.
        unsafe { std::env::set_var("QOS_APPS_DATA_DIR", tmp.path()) };
        let found = find_installed("dev.qos.notes-cli").expect("read");
        unsafe { std::env::remove_var("QOS_APPS_DATA_DIR") };
        let entry = found.expect("dev.qos.notes-cli present in registry");
        assert_eq!(entry.id, "dev.qos.notes-cli");
        assert_eq!(entry.payload_root, payload);
    }

    #[test]
    fn find_installed_returns_none_when_registry_missing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // No registry.toml written.
        unsafe { std::env::set_var("QOS_APPS_DATA_DIR", tmp.path()) };
        let found = find_installed("nothing").expect("read");
        unsafe { std::env::remove_var("QOS_APPS_DATA_DIR") };
        assert!(found.is_none());
    }
}
