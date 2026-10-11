//! The registry: one row per installed application, persisted as TOML.
//!
//! [`InstalledPackage`] is the one description of an installed package that
//! every consumer shares — the launcher resolving an app id, the shell
//! listing applications, the app host's capability, the Store and
//! `tilo-install`. A row is keyed by the app id (one installed version per
//! app) and names the signed package root it runs, so an upgrade is a row
//! replacement and a stale row can never point at another package's tree.
//!
//! Rows written before payloads moved into the content store have the same
//! shape with the tree under `payloads/<app-id>`; they stay readable, and
//! [`crate::PackageStore`] moves them into the content-addressed layout once.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use tilo_app_package::PackageId;

/// Whether the user has committed to keeping this package around.
///
/// `Pinned` packages are exempt from size-pressure eviction; `Tried`
/// packages are evicted least recently used first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Pinning {
    Tried,
    Pinned,
}

impl Pinning {
    pub fn as_str(self) -> &'static str {
        match self {
            Pinning::Tried => "tried",
            Pinning::Pinned => "pinned",
        }
    }
}

/// One installed package.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledPackage {
    /// Reverse-DNS app id, from the manifest's `[package].id`.
    pub id: String,
    /// Version from the manifest.
    #[serde(default)]
    pub version: String,
    /// The signed package root (hex, no `tapp:` prefix). Empty only for
    /// hand-written development fixtures; security-sensitive callers must
    /// reject such a row rather than derive trust from a path.
    #[serde(default)]
    pub package_id_hex: String,
    /// The payload tree the launcher binds as the app's root.
    pub payload_root: PathBuf,
    /// The manifest the launcher reads.
    pub manifest_path: PathBuf,
    /// Publisher public key (hex).
    #[serde(default)]
    pub publisher_key_hex: String,
    /// Pinned packages are kept under size pressure.
    #[serde(default)]
    pub pinned: bool,
    /// Unix seconds of install.
    #[serde(default)]
    pub installed_at: u64,
    /// Unix seconds of the last launch (or install); orders eviction.
    #[serde(default)]
    pub last_used: u64,
    /// Attestor id when an attestor chain vouched for the package; empty
    /// for a self-signed one.
    #[serde(default)]
    pub attested_by: String,
    /// PATH commands this package published.
    #[serde(default)]
    pub cli_commands: Vec<String>,
    /// The manifest's declared `tilo-runtime/<v>`, if any.
    #[serde(default)]
    pub runtime: String,
    /// Where the bytes came from (a file or a registry) — provenance, not a
    /// trust input.
    #[serde(default)]
    pub source: String,
    /// The user went back to this version on purpose (a rollback): it keeps
    /// running even when the system image ships a newer one, until the next
    /// update. Without it the newer of the user's and the image's copy runs.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub held: bool,
    /// Bytes of the payload tree, recorded once at install so listing a
    /// package never walks its tree. `0` for a row written before it was
    /// recorded; such a row is measured on disk instead.
    #[serde(default)]
    pub payload_bytes: u64,
    /// True for a package of the read-only system store. Not persisted:
    /// it describes which store a row was read from.
    #[serde(skip)]
    pub system: bool,
}

impl InstalledPackage {
    /// The signed package root, when the row names a well-formed one.
    pub fn package_id(&self) -> Option<PackageId> {
        crate::layout::parse_id_hex(&self.package_id_hex)
    }

    pub fn pinning(&self) -> Pinning {
        if self.pinned {
            Pinning::Pinned
        } else {
            Pinning::Tried
        }
    }

    /// Whether the user's store owns this package (and so may pin or
    /// remove it).
    pub fn user_managed(&self) -> bool {
        !self.system
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct RegistryFile {
    #[serde(default)]
    entry: Vec<InstalledPackage>,
}

/// The rows of one `registry.toml`, in file order.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Registry {
    entries: Vec<InstalledPackage>,
}

#[derive(Debug, Error)]
pub enum RegistryError {
    #[error("reading registry at {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("registry {path}: {detail}")]
    Parse { path: PathBuf, detail: String },
    #[error("writing registry at {path}: {detail}")]
    Write { path: PathBuf, detail: String },
}

impl Registry {
    /// Load `path`. A missing file is an empty registry (nothing installed);
    /// a file that exists but does not parse is an error, because silently
    /// reporting "no apps" would hide the breakage.
    ///
    /// # Errors
    ///
    /// Returns [`RegistryError`] if the file exists but cannot be read or
    /// parsed.
    pub fn load(path: &Path) -> Result<Self, RegistryError> {
        let raw = match std::fs::read_to_string(path) {
            Ok(raw) => raw,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(source) => {
                return Err(RegistryError::Read {
                    path: path.to_path_buf(),
                    source,
                });
            }
        };
        let file: RegistryFile = toml::from_str(&raw).map_err(|error| RegistryError::Parse {
            path: path.to_path_buf(),
            detail: error.to_string(),
        })?;
        Ok(Self {
            entries: file.entry,
        })
    }

    /// Persist to `path` atomically, creating parent directories.
    ///
    /// # Errors
    ///
    /// Returns [`RegistryError::Write`] if encoding or the atomic
    /// replacement fails.
    #[cfg(feature = "store")]
    pub fn save(&self, path: &Path) -> Result<(), RegistryError> {
        let write_error = |detail: String| RegistryError::Write {
            path: path.to_path_buf(),
            detail,
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| write_error(error.to_string()))?;
        }
        let file = RegistryFile {
            entry: self.entries.clone(),
        };
        let text = toml::to_string(&file).map_err(|error| write_error(error.to_string()))?;
        tilo_files::native::atomic::write(path, text.as_bytes())
            .map_err(|error| write_error(error.to_string()))
    }

    pub fn entries(&self) -> &[InstalledPackage] {
        &self.entries
    }

    pub fn into_entries(self) -> Vec<InstalledPackage> {
        self.entries
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The row for `app_id`.
    pub fn find(&self, app_id: &str) -> Option<&InstalledPackage> {
        self.entries.iter().find(|entry| entry.id == app_id)
    }

    /// The row for `app_id`, mutably.
    pub fn find_mut(&mut self, app_id: &str) -> Option<&mut InstalledPackage> {
        self.entries.iter_mut().find(|entry| entry.id == app_id)
    }

    /// The row running package `id`.
    pub fn find_package(&self, id: PackageId) -> Option<&InstalledPackage> {
        let hex = id.to_hex();
        self.entries
            .iter()
            .find(|entry| entry.package_id_hex.eq_ignore_ascii_case(&hex))
    }

    /// Insert `entry`, replacing the row of the same app id in place.
    /// Returns the replaced row.
    pub fn upsert(&mut self, entry: InstalledPackage) -> Option<InstalledPackage> {
        if let Some(slot) = self.entries.iter_mut().find(|row| row.id == entry.id) {
            return Some(std::mem::replace(slot, entry));
        }
        self.entries.push(entry);
        None
    }

    /// Drop the row for `app_id`.
    pub fn remove(&mut self, app_id: &str) -> Option<InstalledPackage> {
        let index = self.entries.iter().position(|entry| entry.id == app_id)?;
        Some(self.entries.remove(index))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_rows_load_parse_and_upsert() {
        // phase: a_legacy_row_parses_with_defaults
        {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("registry.toml");
            std::fs::write(
            &path,
            "[[entry]]\nid = 'so.tilo.notes'\npayload_root = '/p'\nmanifest_path = '/p/manifest.toml'\n\
             unknown_future_field = 3\n",
        )
        .unwrap();
            let registry = Registry::load(&path).unwrap();
            let row = registry.find("so.tilo.notes").unwrap();
            assert_eq!(row.package_id(), None);
            assert_eq!(row.pinning(), Pinning::Tried);
            assert!(row.user_managed());
        }
        // phase: a_missing_registry_is_empty_and_a_broken_one_is_an_error
        {
            let dir = tempfile::tempdir().unwrap();
            assert!(
                Registry::load(&dir.path().join("absent.toml"))
                    .unwrap()
                    .is_empty()
            );
            let broken = dir.path().join("broken.toml");
            std::fs::write(&broken, "[[entry]]\nid = 3\n").unwrap();
            assert!(matches!(
                Registry::load(&broken),
                Err(RegistryError::Parse { .. })
            ));
        }
        // phase: upsert_replaces_the_row_of_the_same_app
        {
            let row = |version: &str| InstalledPackage {
                id: "so.tilo.notes".into(),
                version: version.into(),
                package_id_hex: String::new(),
                payload_root: PathBuf::from("/p"),
                manifest_path: PathBuf::from("/m"),
                publisher_key_hex: String::new(),
                pinned: false,
                installed_at: 0,
                last_used: 0,
                attested_by: String::new(),
                cli_commands: Vec::new(),
                runtime: String::new(),
                source: String::new(),
                held: false,
                payload_bytes: 0,
                system: false,
            };
            let mut registry = Registry::default();
            assert!(registry.upsert(row("1")).is_none());
            let replaced = registry.upsert(row("2")).unwrap();
            assert_eq!(replaced.version, "1");
            assert_eq!(registry.entries().len(), 1);
            assert_eq!(registry.find("so.tilo.notes").unwrap().version, "2");
        }
    }
}
