//! The registry: one row per installed application, persisted as TOML.

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledPackage {
    pub id: String,
    #[serde(default)]
    pub version: String,
    /// The signed package root (hex, no `tapp:` prefix). Empty only for
    /// hand-written development fixtures; security-sensitive callers must
    /// reject such a row rather than derive trust from a path.
    #[serde(default)]
    pub package_id_hex: String,
    pub payload_root: PathBuf,
    pub manifest_path: PathBuf,
    /// Publisher public key (hex).
    #[serde(default)]
    pub publisher_key_hex: String,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default)]
    pub installed_at: u64,
    /// Unix seconds of the last launch (or the first install; an update
    /// keeps it); orders eviction.
    #[serde(default)]
    pub last_used: u64,
    /// Attestor id when an attestor chain vouched for the package; empty
    /// for a self-signed one.
    #[serde(default)]
    pub attested_by: String,
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

    pub fn user_managed(&self) -> bool {
        !self.system
    }

    /// A row for a package whose payload and signed evidence live outside the store, at the paths
    /// given: the imager's own capture worker and the launcher's qualification fixtures lay a
    /// package out that way. Only the signed root, the payload and the manifest are named here, so a
    /// caller that needs more fills the remaining fields before verifying.
    pub fn detached(
        id: impl Into<String>,
        package_id_hex: String,
        payload_root: PathBuf,
        manifest_path: PathBuf,
    ) -> Self {
        Self {
            id: id.into(),
            version: String::new(),
            package_id_hex,
            payload_root,
            manifest_path,
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
        }
    }
}

/// An app installed under an id another app of the same publisher names as its earlier one
/// (`[package] previously`), which that app took the place of when it was installed: `id` is the
/// earlier id, `by` the app that took it over.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TakenOver {
    pub id: String,
    pub by: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
struct RegistryFile {
    #[serde(default)]
    entry: Vec<InstalledPackage>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    taken_over: Vec<TakenOver>,
}

/// The rows of one `registry.toml`, in file order.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Registry {
    entries: Vec<InstalledPackage>,
    taken_over: Vec<TakenOver>,
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
            taken_over: file.taken_over,
        })
    }

    /// [`Self::load`] of the registry a store at `layout` keeps, each row resolved in that store
    /// ([`crate::layout::Layout::resolve`]): a store read where it stands names its own payloads,
    /// wherever the rows were first installed.
    ///
    /// # Errors
    ///
    /// As [`Self::load`].
    pub fn load_in(layout: &crate::layout::Layout) -> Result<Self, RegistryError> {
        let mut registry = Self::load(&layout.registry())?;
        for entry in &mut registry.entries {
            layout.resolve(entry);
        }
        Ok(registry)
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
            taken_over: self.taken_over.clone(),
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

    pub fn find(&self, app_id: &str) -> Option<&InstalledPackage> {
        self.entries.iter().find(|entry| entry.id == app_id)
    }

    pub fn find_mut(&mut self, app_id: &str) -> Option<&mut InstalledPackage> {
        self.entries.iter_mut().find(|entry| entry.id == app_id)
    }

    pub fn find_package(&self, id: PackageId) -> Option<&InstalledPackage> {
        let hex = id.to_hex();
        self.entries
            .iter()
            .find(|entry| entry.package_id_hex.eq_ignore_ascii_case(&hex))
    }

    pub fn upsert(&mut self, entry: InstalledPackage) -> Option<InstalledPackage> {
        if let Some(slot) = self.entries.iter_mut().find(|row| row.id == entry.id) {
            return Some(std::mem::replace(slot, entry));
        }
        self.entries.push(entry);
        None
    }

    pub fn remove(&mut self, app_id: &str) -> Option<InstalledPackage> {
        let index = self.entries.iter().position(|entry| entry.id == app_id)?;
        self.taken_over.retain(|taken| taken.by != app_id);
        Some(self.entries.remove(index))
    }

    pub fn taken_over(&self) -> &[TakenOver] {
        &self.taken_over
    }

    /// The installed app that took over the earlier id `id`, when one did.
    pub fn taken_over_by(&self, id: &str) -> Option<&str> {
        self.taken_over
            .iter()
            .find(|taken| taken.id == id)
            .map(|taken| taken.by.as_str())
    }

    pub fn take_over(&mut self, id: &str, by: &str) {
        self.taken_over.retain(|taken| taken.id != id);
        self.taken_over.push(TakenOver {
            id: id.to_owned(),
            by: by.to_owned(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_rows_load_and_parse_even_with_an_unknown_future_field() {
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
    }

    /// An image names each app's tree by its payload (`trees/<payload root hash>`), not by its id;
    /// the image's store read where it stands, or a copy of it, finds that tree, never a
    /// `pkgs/<id>` the image no longer has.
    #[test]
    fn a_tree_named_by_its_payload_is_found_where_the_row_put_it() {
        let dir = tempfile::tempdir().unwrap();
        let image = crate::layout::Layout::new(dir.path().join("image/tilo-apps"));
        let id = PackageId::from_bytes([9; 32]);
        let hex = id.to_hex();
        let tree = image.root().join("trees").join("ab".repeat(32));
        std::fs::create_dir_all(&tree).unwrap();
        std::fs::create_dir_all(image.meta_dir(id)).unwrap();
        std::fs::write(
            image.registry(),
            format!(
                "[[entry]]\nid = 'so.tilo.notes'\npackage_id_hex = {hex:?}\n\
                 payload_root = {:?}\nmanifest_path = {:?}\n",
                tree.display().to_string(),
                image.manifest(id).display().to_string(),
            ),
        )
        .unwrap();
        let row = Registry::load_in(&image)
            .unwrap()
            .find("so.tilo.notes")
            .cloned()
            .expect("the row");
        assert_eq!(row.payload_root, tree);
        assert_eq!(row.manifest_path, image.manifest(id));

        let copy = crate::layout::Layout::new(dir.path().join("device/tilo-apps"));
        std::fs::create_dir_all(copy.root()).unwrap();
        std::fs::copy(image.registry(), copy.registry()).unwrap();
        let row = Registry::load_in(&copy)
            .unwrap()
            .find("so.tilo.notes")
            .cloned()
            .expect("the row");
        assert_eq!(
            row.payload_root,
            copy.root().join("trees").join("ab".repeat(32))
        );
        assert_eq!(row.manifest_path, copy.manifest(id));
    }

    /// A registry read where its store stands names that store's payloads, not the store the rows
    /// were installed with: a store copied whole (a device's daemon reading what an installer
    /// staged, a walk's device reading a session's store) launches from where it is read, and from
    /// nothing the copy cannot reach.
    #[test]
    fn a_registry_resolves_its_rows_in_the_store_it_lives_in() {
        let dir = tempfile::tempdir().unwrap();
        let layout = crate::layout::Layout::new(dir.path().join("device/tilo-apps"));
        let id = PackageId::from_bytes([7; 32]);
        let hex = id.to_hex();
        std::fs::create_dir_all(layout.packages()).unwrap();
        std::fs::write(
            layout.registry(),
            format!(
                "[[entry]]\nid = 'so.tilo.notes'\npackage_id_hex = {hex:?}\n\
                 payload_root = {:?}\nmanifest_path = {:?}\n",
                format!("/person/state/apps/pkgs/{hex}"),
                format!("/person/state/apps/pkgs/{hex}.pkg/manifest.toml"),
            ),
        )
        .unwrap();
        let row = Registry::load_in(&layout)
            .unwrap()
            .find("so.tilo.notes")
            .cloned()
            .expect("the row");
        assert_eq!(row.payload_root, layout.payload(id));
        assert_eq!(row.manifest_path, layout.manifest(id));
        // The file itself is untouched: it still records where the copy came from.
        assert!(
            Registry::load(&layout.registry())
                .unwrap()
                .find("so.tilo.notes")
                .unwrap()
                .payload_root
                .starts_with("/person/state/apps")
        );
    }
}
