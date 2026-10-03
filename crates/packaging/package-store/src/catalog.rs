//! What this session can run: the system store and the user's store,
//! merged.
//!
//! Both stores describe packages the same way — being preinstalled is a
//! matter of *where the payload lives*, not what the package may do, so a
//! system row is granted exactly what a user row is. A user install
//! shadows a system package with the same app id.

use std::path::{Path, PathBuf};

use thiserror::Error;

use tilo_app_package::PackageId;

use crate::layout::{self, Layout};
use crate::registry::{InstalledPackage, Registry, RegistryError};

#[derive(Debug, Error)]
pub enum CatalogError {
    #[error("HOME / XDG_DATA_HOME not set — cannot locate the app store")]
    NoHome,
    #[error(transparent)]
    Registry(#[from] RegistryError),
    #[error("system package {id} lies outside {root}")]
    Escapes { id: String, root: PathBuf },
}

/// The system store's location: its registry directory and the other root
/// its payload trees may live under.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SystemStore {
    pub data_dir: PathBuf,
    pub payload_dir: PathBuf,
}

impl SystemStore {
    /// The system store the environment names, if any; see
    /// [`layout::system_data_dir`].
    pub fn from_env() -> Option<Self> {
        layout::system_data_dir().map(|data_dir| Self {
            data_dir,
            payload_dir: layout::system_payload_dir(),
        })
    }
}

/// The stores one session reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Catalog {
    user: Layout,
    system: Option<SystemStore>,
}

impl Catalog {
    /// Explicit stores, for daemons that resolve locations once and tests.
    pub fn at(user_root: impl Into<PathBuf>, system: Option<SystemStore>) -> Self {
        Self {
            user: Layout::new(user_root),
            system,
        }
    }

    /// The stores the environment names; see [`layout::default_user_root`]
    /// and [`layout::system_data_dir`].
    ///
    /// # Errors
    ///
    /// Returns [`CatalogError::NoHome`] when no user store can be located.
    pub fn from_env() -> Result<Self, CatalogError> {
        let user = layout::default_user_root().ok_or(CatalogError::NoHome)?;
        Ok(Self::at(user, SystemStore::from_env()))
    }

    pub fn user(&self) -> &Layout {
        &self.user
    }

    pub fn system(&self) -> Option<&SystemStore> {
        self.system.as_ref()
    }

    /// Every package either store knows about: the system store first, then
    /// the user's, with a user row replacing a system row of the same app id
    /// in place (so the system store's ordering stays stable).
    ///
    /// # Errors
    ///
    /// Returns [`CatalogError`] if either registry exists but cannot be read
    /// or parsed, or a system row points outside the system store.
    pub fn list(&self) -> Result<Vec<InstalledPackage>, CatalogError> {
        let mut out = match &self.system {
            Some(system) => system_packages(system)?,
            None => Vec::new(),
        };
        for entry in Registry::load(&self.user.registry())?.into_entries() {
            match out.iter_mut().find(|row| row.id == entry.id) {
                Some(slot) => *slot = entry,
                None => out.push(entry),
            }
        }
        Ok(out)
    }

    /// The package that runs for `app_id`.
    ///
    /// # Errors
    ///
    /// See [`Catalog::list`].
    pub fn find(&self, app_id: &str) -> Result<Option<InstalledPackage>, CatalogError> {
        Ok(self.list()?.into_iter().find(|row| row.id == app_id))
    }

    /// The visible package whose signed root is `id`. A shadowed system
    /// package is not visible.
    ///
    /// # Errors
    ///
    /// See [`Catalog::list`].
    pub fn find_package(&self, id: PackageId) -> Result<Option<InstalledPackage>, CatalogError> {
        Ok(self
            .list()?
            .into_iter()
            .find(|row| row.package_id() == Some(id)))
    }
}

fn is_within(root: &Path, path: &Path) -> bool {
    match (std::fs::canonicalize(root), std::fs::canonicalize(path)) {
        (Ok(root), Ok(path)) => path.starts_with(root),
        _ => false,
    }
}

/// A developer session serves a complete staged system store from another
/// directory without editing its release-shaped registry: translate the
/// canonical [`layout::SYSTEM_PAYLOAD_DIR`] prefix of a row's paths to the
/// store's payload root. Rows elsewhere are left for the escape check.
fn relocate_system_row(system: &SystemStore, row: &mut InstalledPackage) {
    let canonical = Path::new(layout::SYSTEM_PAYLOAD_DIR);
    if system.payload_dir == canonical {
        return;
    }
    for path in [&mut row.payload_root, &mut row.manifest_path] {
        if let Ok(suffix) = path.strip_prefix(canonical) {
            *path = system.payload_dir.join(suffix);
        }
    }
}

fn system_packages(system: &SystemStore) -> Result<Vec<InstalledPackage>, CatalogError> {
    let rows = Registry::load(&system.data_dir.join(layout::REGISTRY_FILE))?.into_entries();
    let mut out = Vec::with_capacity(rows.len());
    for mut row in rows {
        relocate_system_row(system, &mut row);
        // The system registry ships inside the image; a row that names a
        // tree elsewhere is a broken or tampered image, not a package.
        for path in [&row.payload_root, &row.manifest_path] {
            if path.exists()
                && !is_within(&system.data_dir, path)
                && !is_within(&system.payload_dir, path)
            {
                return Err(CatalogError::Escapes {
                    id: row.id.clone(),
                    root: system.data_dir.clone(),
                });
            }
        }
        row.system = true;
        out.push(row);
    }
    Ok(out)
}

/// Every installed package this session can run; see [`Catalog::list`].
///
/// # Errors
///
/// See [`Catalog::from_env`] and [`Catalog::list`].
pub fn list_installed() -> Result<Vec<InstalledPackage>, CatalogError> {
    Catalog::from_env()?.list()
}

/// The package that runs for `app_id`; see [`Catalog::find`].
///
/// # Errors
///
/// See [`Catalog::from_env`] and [`Catalog::list`].
pub fn find_installed(app_id: &str) -> Result<Option<InstalledPackage>, CatalogError> {
    Catalog::from_env()?.find(app_id)
}

/// `so.tilo.text-editor` → `Text Editor`.
///
/// The fallback display name for a package with none — the trailing id
/// component is the only signal there is. Separators become spaces and each
/// word is title-cased. Lives here because the desktop derives it for an
/// installed package with no manifest name, and the catalogue for an
/// available one with no registry name — the same package must not be
/// called two different things.
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt::Write as _;

    /// Write a `registry.toml` naming `ids`, with a payload dir and manifest
    /// on disk for each, and return the directory.
    fn store_with(ids: &[&str]) -> tempfile::TempDir {
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

    fn catalog(system: &Path, user: &Path) -> Catalog {
        Catalog::at(
            user,
            Some(SystemStore {
                data_dir: system.to_path_buf(),
                payload_dir: system.join("lib"),
            }),
        )
    }

    #[test]
    fn system_and_user_stores_merge_into_one_catalog() {
        // phase: system_store_entries_are_listed_without_any_user_install
        {
            // The preinstalled case: an image ships a system registry and the user
            // never installed anything. The palette must still have something to list.
            let system = store_with(&["so.tilo.notes", "so.tilo.calculator"]);
            let user = tempfile::tempdir().unwrap();
            let catalog = catalog(system.path(), user.path());
            let all = catalog.list().unwrap();
            let ids: Vec<&str> = all.iter().map(|e| e.id.as_str()).collect();
            assert_eq!(ids, vec!["so.tilo.notes", "so.tilo.calculator"]);
            assert!(all.iter().all(|row| row.system && !row.user_managed()));
            let found = catalog.find("so.tilo.notes").unwrap().unwrap();
            assert!(found.payload_root.starts_with(system.path()));
        }
        // phase: both_stores_merge_and_a_user_install_shadows_the_system_one
        {
            let system = store_with(&["so.tilo.notes", "so.tilo.calculator"]);
            let user = store_with(&["so.tilo.notes", "so.tilo.mine"]);
            let all = catalog(system.path(), user.path()).list().unwrap();
            let ids: Vec<&str> = all.iter().map(|e| e.id.as_str()).collect();
            assert_eq!(
                ids,
                vec!["so.tilo.notes", "so.tilo.calculator", "so.tilo.mine"],
                "one row per id: the shadowed system entry must not appear twice",
            );
            let notes = all.iter().find(|e| e.id == "so.tilo.notes").unwrap();
            assert!(notes.payload_root.starts_with(user.path()));
            assert!(notes.user_managed());
            let calc = all.iter().find(|e| e.id == "so.tilo.calculator").unwrap();
            assert!(calc.payload_root.starts_with(system.path()));
        }
        // phase: relocated_system_store_uses_the_developer_payload_root
        {
            let system = tempfile::tempdir().expect("system registry");
            let user = tempfile::tempdir().expect("user registry");
            let relocated = tempfile::tempdir().expect("relocated payload");
            let payload = relocated.path().join("payloads/dev.tilo.chat");
            std::fs::create_dir_all(&payload).unwrap();
            let manifest = payload.join("manifest.toml");
            std::fs::write(&manifest, "").unwrap();
            std::fs::write(
            system.path().join("registry.toml"),
            "[[entry]]\nid='dev.tilo.chat'\npayload_root='/usr/lib/tilo-apps/payloads/dev.tilo.chat'\nmanifest_path='/usr/lib/tilo-apps/payloads/dev.tilo.chat/manifest.toml'\n",
        )
        .unwrap();
            let catalog = Catalog::at(
                user.path(),
                Some(SystemStore {
                    data_dir: system.path().to_path_buf(),
                    payload_dir: relocated.path().to_path_buf(),
                }),
            );
            let entry = catalog
                .find("dev.tilo.chat")
                .expect("registry readable")
                .expect("chat installed");
            assert_eq!(entry.payload_root, payload);
            assert_eq!(entry.manifest_path, manifest);
        }
        // phase: a_missing_system_store_is_not_an_error
        {
            // A plain dev machine has no /usr/share/tilo-apps — that reads as
            // "nothing preinstalled", not a failure hiding the user's own installs.
            let system = tempfile::tempdir().unwrap();
            let user = store_with(&["so.tilo.mine"]);
            let all = catalog(system.path(), user.path()).list().unwrap();
            let ids: Vec<&str> = all.iter().map(|e| e.id.as_str()).collect();
            assert_eq!(ids, vec!["so.tilo.mine"]);
        }
        // phase: a_system_row_pointing_outside_the_image_is_refused
        {
            let system = tempfile::tempdir().unwrap();
            let outside = tempfile::tempdir().unwrap();
            std::fs::write(
                system.path().join("registry.toml"),
                format!(
                    "[[entry]]\nid = 'so.tilo.x'\npayload_root = {:?}\nmanifest_path = {:?}\n",
                    outside.path().display().to_string(),
                    outside.path().display().to_string()
                ),
            )
            .unwrap();
            let user = tempfile::tempdir().unwrap();
            assert!(matches!(
                catalog(system.path(), user.path()).list(),
                Err(CatalogError::Escapes { .. })
            ));
        }
    }

    #[test]
    fn the_environment_names_the_stores() {
        let user = store_with(&["so.tilo.alpha", "so.tilo.beta"]);
        let mut env = tilo_test_env::ScopedEnv::new();
        env.remove("TILO_APPS_SYSTEM_DIR")
            .set("TILO_APPS_DATA_DIR", user.path());
        let all = list_installed().unwrap();
        let ids: Vec<&str> = all.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, vec!["so.tilo.alpha", "so.tilo.beta"]);
        assert!(find_installed("so.tilo.beta").unwrap().is_some());
        assert!(find_installed("nothing").unwrap().is_none());
    }

    #[test]
    fn display_names_come_from_the_last_id_component() {
        assert_eq!(
            display_name_for_package_id("so.tilo.text-editor"),
            "Text Editor"
        );
        assert_eq!(display_name_for_package_id("calc"), "Calc");
        assert_eq!(display_name_for_package_id("so.tilo.a_b"), "A B");
    }
}
