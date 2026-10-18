//! What this session can run: the system store and the user's store,
//! merged.
//!
//! Both stores describe packages the same way — being preinstalled is a
//! matter of *where the payload lives*, not what the package may do, so a
//! system row is granted exactly what a user row is. The system store is
//! the layer of apps the image installed; updating one installs the newer
//! version into the user's store, which shadows it (see
//! [`runs_over_image`]) as long as it fits the OS: a user copy requiring a
//! system interface version the image does not serve never runs over the
//! image's copy, which matches its own image by construction (see
//! [`crate::interfaces`]).

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use thiserror::Error;

use tilo_app_package::PackageId;
use tilo_app_package::interfaces::{ProvidedInterfaces, Unmet};

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
    /// The system interfaces the OS under this store serves; `None` when it
    /// declares none, and then every user copy is judged to fit.
    pub interfaces: Option<ProvidedInterfaces>,
}

impl SystemStore {
    /// The system store the environment names, if any, with the OS's
    /// interface declaration; see [`layout::system_data_dir`] and
    /// [`crate::interfaces::system_interfaces`].
    pub fn from_env() -> Option<Self> {
        layout::system_data_dir().map(|data_dir| Self {
            data_dir,
            payload_dir: layout::system_payload_dir(),
            interfaces: crate::interfaces::system_interfaces(),
        })
    }
}

/// The stores one session reads.
///
/// The system store ships inside the immutable image, so its rows are read
/// and checked once per `Catalog` and kept; only the user's registry is
/// re-read on every lookup. A long-lived holder (a daemon) therefore pays
/// for the system store once, and a per-call `Catalog` once per call.
#[derive(Clone, Debug)]
pub struct Catalog {
    user: Layout,
    system: Option<SystemStore>,
    /// The system store's rows, once read successfully. A failed read is
    /// not kept: the next lookup reports it again.
    system_rows: OnceLock<Vec<InstalledPackage>>,
}

impl PartialEq for Catalog {
    /// Two catalogues are equal when they read the same stores, whatever
    /// either has cached.
    fn eq(&self, other: &Self) -> bool {
        self.user == other.user && self.system == other.system
    }
}

impl Eq for Catalog {}

impl Catalog {
    /// Explicit stores, for daemons that resolve locations once and tests.
    pub fn at(user_root: impl Into<PathBuf>, system: Option<SystemStore>) -> Self {
        Self {
            user: Layout::new(user_root),
            system,
            system_rows: OnceLock::new(),
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
    /// the user's. Where both hold an app, the one that runs takes the
    /// system row's place (so the system store's ordering stays stable); see
    /// [`runs_over_image`] for which.
    ///
    /// # Errors
    ///
    /// Returns [`CatalogError`] if either registry exists but cannot be read
    /// or parsed, or a system row points outside the system store.
    pub fn list(&self) -> Result<Vec<InstalledPackage>, CatalogError> {
        let mut out = self.image_packages()?;
        for entry in Registry::load(&self.user.registry())?.into_entries() {
            match out.iter_mut().find(|row| row.id == entry.id) {
                Some(slot) => {
                    if runs_over_image(&entry, slot) && self.unmet(&entry).is_empty() {
                        *slot = entry;
                    }
                }
                None => out.push(entry),
            }
        }
        Ok(out)
    }

    /// Both copies of `app_id`: the user's store's and the system image's.
    ///
    /// # Errors
    ///
    /// See [`Catalog::list`].
    pub fn layers(&self, app_id: &str) -> Result<Layers, CatalogError> {
        let user = Registry::load(&self.user.registry())?.find(app_id).cloned();
        let image = self
            .image_packages()?
            .into_iter()
            .find(|row| row.id == app_id);
        let user_unmet = user.as_ref().map_or_else(Vec::new, |row| self.unmet(row));
        Ok(Layers {
            user,
            image,
            user_unmet,
        })
    }

    /// The interfaces `row`'s package requires that this OS does not serve.
    /// Empty when it fits, or when the OS declares nothing.
    pub fn unmet(&self, row: &InstalledPackage) -> Vec<Unmet> {
        crate::interfaces::unmet_at(self.interfaces(), &row.manifest_path)
    }

    /// The system interfaces the OS serves, as the system store's image
    /// declares them.
    pub fn interfaces(&self) -> Option<&ProvidedInterfaces> {
        self.system
            .as_ref()
            .and_then(|system| system.interfaces.as_ref())
    }

    /// The user's copies the system image now ships a newer version of and
    /// that were not held back on purpose: nothing runs them any more.
    ///
    /// # Errors
    ///
    /// See [`Catalog::list`].
    pub fn superseded(&self) -> Result<Vec<InstalledPackage>, CatalogError> {
        let image = self.image_packages()?;
        Ok(Registry::load(&self.user.registry())?
            .into_entries()
            .into_iter()
            .filter(|entry| {
                image
                    .iter()
                    .any(|row| row.id == entry.id && !runs_over_image(entry, row))
            })
            .collect())
    }

    fn image_packages(&self) -> Result<Vec<InstalledPackage>, CatalogError> {
        let Some(system) = &self.system else {
            return Ok(Vec::new());
        };
        if let Some(rows) = self.system_rows.get() {
            return Ok(rows.clone());
        }
        let rows = system_packages(system)?;
        Ok(self.system_rows.get_or_init(|| rows).clone())
    }

    /// The package that runs for `app_id`.
    ///
    /// # Errors
    ///
    /// See [`Catalog::layers`].
    pub fn find(&self, app_id: &str) -> Result<Option<InstalledPackage>, CatalogError> {
        Ok(self.layers(app_id)?.running().cloned())
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

/// One app's copy in each store.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Layers {
    /// The copy the user installed or updated to.
    pub user: Option<InstalledPackage>,
    /// The copy the system image ships.
    pub image: Option<InstalledPackage>,
    /// The system interfaces the user's copy requires that this OS does not
    /// serve. While any are listed the image's copy runs, if there is one.
    pub user_unmet: Vec<Unmet>,
}

impl Layers {
    /// The copy that runs.
    pub fn running(&self) -> Option<&InstalledPackage> {
        match (&self.user, &self.image) {
            (Some(user), Some(image))
                if !runs_over_image(user, image) || !self.user_unmet.is_empty() =>
            {
                Some(image)
            }
            (Some(user), _) => Some(user),
            (None, image) => image.as_ref(),
        }
    }
}

/// Whether the user's copy of an app runs instead of the system image's.
///
/// The image's apps are the first installed version of each, updated the
/// way any package is: an update installs the newer version into the user's
/// store, where it shadows the image's. The newer of the two runs, so an
/// image update never takes an app back to an older version than the user
/// updated to, and an update the image already carries does not leave the
/// user stuck on an older copy. A copy the user rolled back to is
/// [`held`](InstalledPackage::held) and runs whatever the image ships, until
/// the next update.
pub fn runs_over_image(user: &InstalledPackage, image: &InstalledPackage) -> bool {
    user.held
        || tilo_app_package::version::compare(&user.version, &image.version)
            != std::cmp::Ordering::Less
}

/// Whether an existing `path` resolves into one of `roots` (already
/// canonical). A path that does not exist names no tree, so it is not an
/// escape; one that exists but cannot be resolved is.
///
/// The roots are resolved once per registry read, not once per row: every
/// launch reads the registry, and resolving both store roots again for each
/// of the image's packages was most of that read.
fn escapes(roots: &[Option<PathBuf>], path: &Path) -> bool {
    match std::fs::canonicalize(path) {
        Ok(path) => !roots.iter().flatten().any(|root| path.starts_with(root)),
        Err(_) => path.exists(),
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
    let roots = [
        std::fs::canonicalize(&system.data_dir).ok(),
        std::fs::canonicalize(&system.payload_dir).ok(),
    ];
    for mut row in rows {
        relocate_system_row(system, &mut row);
        // The system registry ships inside the image; a row that names a
        // tree elsewhere is a broken or tampered image, not a package.
        for path in [&row.payload_root, &row.manifest_path] {
            if escapes(&roots, path) {
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
                interfaces: None,
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
                    interfaces: None,
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

    #[cfg(unix)]
    #[test]
    fn a_system_row_resolving_outside_through_a_link_is_refused() {
        // Named inside the store, but a link there resolves elsewhere:
        // where a path leads is what counts, not how it is spelled.
        let system = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let link = system.path().join("pkgs");
        std::os::unix::fs::symlink(outside.path(), &link).unwrap();
        std::fs::write(
            system.path().join("registry.toml"),
            format!(
                "[[entry]]\nid = 'so.tilo.x'\npayload_root = {:?}\nmanifest_path = {:?}\n",
                link.display().to_string(),
                link.join("absent.toml").display().to_string()
            ),
        )
        .unwrap();
        let user = tempfile::tempdir().unwrap();
        assert!(matches!(
            catalog(system.path(), user.path()).list(),
            Err(CatalogError::Escapes { .. })
        ));
    }

    #[test]
    fn a_long_lived_catalog_reads_the_system_registry_once() {
        let system = store_with(&["so.tilo.notes", "so.tilo.calculator"]);
        let user = tempfile::tempdir().unwrap();
        let long_lived = catalog(system.path(), user.path());
        let ids = |rows: Vec<InstalledPackage>| -> Vec<String> {
            rows.into_iter().map(|row| row.id).collect()
        };
        assert_eq!(
            ids(long_lived.list().unwrap()),
            ["so.tilo.notes", "so.tilo.calculator"]
        );

        // Were the system registry read again, this would now fail to parse.
        std::fs::write(system.path().join("registry.toml"), "[[entry]]\nid = 3\n").unwrap();
        // The user's registry is still read on every lookup.
        let mine = store_with(&["so.tilo.mine"]);
        std::fs::copy(
            mine.path().join("registry.toml"),
            user.path().join("registry.toml"),
        )
        .unwrap();
        assert_eq!(
            ids(long_lived.list().unwrap()),
            ["so.tilo.notes", "so.tilo.calculator", "so.tilo.mine"]
        );
        assert!(long_lived.find("so.tilo.calculator").unwrap().is_some());

        // A fresh catalogue reads the store as it now is.
        assert!(matches!(
            catalog(system.path(), user.path()).list(),
            Err(CatalogError::Registry(RegistryError::Parse { .. }))
        ));
    }

    /// A store whose rows carry versions, and `held` where asked.
    fn versioned_store(rows: &[(&str, &str, bool)]) -> tempfile::TempDir {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut toml_src = String::new();
        for (id, version, held) in rows {
            let payload = tmp.path().join("payloads").join(id);
            std::fs::create_dir_all(&payload).unwrap();
            let manifest = payload.join("manifest.toml");
            std::fs::write(&manifest, "").unwrap();
            let _ = write!(
                toml_src,
                "[[entry]]\nid = '{id}'\nversion = '{version}'\nheld = {held}\n\
                 payload_root = {:?}\nmanifest_path = {:?}\n",
                payload.display().to_string(),
                manifest.display().to_string()
            );
        }
        std::fs::write(tmp.path().join("registry.toml"), toml_src).unwrap();
        tmp
    }

    #[test]
    fn the_newer_copy_of_an_image_app_runs_unless_the_user_held_one() {
        let system = versioned_store(&[
            ("so.tilo.settings", "0.1.10", false),
            ("so.tilo.notes", "0.1.10", false),
            ("so.tilo.files", "0.1.10", false),
            ("so.tilo.clocks", "0.1.10", false),
        ]);
        let user = versioned_store(&[
            // Updated past the image.
            ("so.tilo.settings", "0.1.12", false),
            // The image has since shipped a newer one.
            ("so.tilo.notes", "0.1.9", false),
            // Rolled back below the image on purpose.
            ("so.tilo.files", "0.1.8", true),
        ]);
        let catalog = catalog(system.path(), user.path());
        let running = |id: &str| {
            let row = catalog.find(id).unwrap().unwrap();
            (row.version, row.system)
        };
        assert_eq!(running("so.tilo.settings"), ("0.1.12".into(), false));
        assert_eq!(
            running("so.tilo.notes"),
            ("0.1.10".into(), true),
            "an image update is not hidden behind an older user copy",
        );
        assert_eq!(running("so.tilo.files"), ("0.1.8".into(), false));
        assert_eq!(running("so.tilo.clocks"), ("0.1.10".into(), true));
        let ids: Vec<String> = catalog.list().unwrap().into_iter().map(|r| r.id).collect();
        assert_eq!(
            ids,
            [
                "so.tilo.settings",
                "so.tilo.notes",
                "so.tilo.files",
                "so.tilo.clocks"
            ],
            "one row per app, in the image's order",
        );

        let superseded: Vec<String> = catalog
            .superseded()
            .unwrap()
            .into_iter()
            .map(|r| r.id)
            .collect();
        assert_eq!(superseded, ["so.tilo.notes"]);

        let layers = catalog.layers("so.tilo.notes").unwrap();
        assert_eq!(layers.user.as_ref().unwrap().version, "0.1.9");
        assert_eq!(layers.image.as_ref().unwrap().version, "0.1.10");
        assert_eq!(layers.running(), layers.image.as_ref());
        let layers = catalog.layers("so.tilo.settings").unwrap();
        assert_eq!(layers.running(), layers.user.as_ref());
        assert_eq!(catalog.layers("so.tilo.none").unwrap(), Layers::default());
    }

    #[test]
    fn a_user_copy_the_os_cannot_serve_never_runs_over_the_image() {
        let system = versioned_store(&[("so.tilo.settings", "0.1.10", false)]);
        let user = versioned_store(&[("so.tilo.settings", "0.2.0", false)]);
        let manifest = |store: &Path, system_version: u32| {
            std::fs::write(
                store.join("payloads/so.tilo.settings/manifest.toml"),
                format!(
                    "[package]\nid = 'so.tilo.settings'\nversion = '0'\nkind = 'app'\n\
                     [publisher]\nidentity = 'ed25519:{zero}'\nsignature = ''\n\
                     [sandbox]\nnamespaces = []\nseccomp = 'tilo-default'\nlandlock = 'strict'\n\
                     [sandbox.net]\nmode = 'off'\n\
                     [sandbox.services]\nsystem = 'required@{system_version}'\n",
                    zero = "0".repeat(64)
                ),
            )
            .unwrap();
        };
        manifest(system.path(), 1);
        manifest(user.path(), 2);
        let on_os = |versions: &[u32]| {
            Catalog::at(
                user.path(),
                Some(SystemStore {
                    data_dir: system.path().to_path_buf(),
                    payload_dir: system.path().join("lib"),
                    interfaces: Some(ProvidedInterfaces::new([("system", versions)])),
                }),
            )
        };

        // An OS serving the update's contract runs the update.
        let newer_os = on_os(&[1, 2]);
        let row = newer_os.find("so.tilo.settings").unwrap().unwrap();
        assert_eq!((row.version.as_str(), row.system), ("0.2.0", false));
        assert!(
            newer_os
                .layers("so.tilo.settings")
                .unwrap()
                .user_unmet
                .is_empty()
        );

        // On an OS that serves only system@1 the image's copy runs instead,
        // and the reason is there to show.
        let older_os = on_os(&[1]);
        let row = older_os.find("so.tilo.settings").unwrap().unwrap();
        assert_eq!((row.version.as_str(), row.system), ("0.1.10", true));
        let layers = older_os.layers("so.tilo.settings").unwrap();
        assert_eq!(layers.running(), layers.image.as_ref());
        assert_eq!(
            layers.user_unmet[0].to_string(),
            "system@2 (this system provides system@1)"
        );
        // Still the user's to remove.
        assert_eq!(layers.user.as_ref().unwrap().version, "0.2.0");

        // A rollback hold does not override the contract either.
        let held = versioned_store(&[("so.tilo.settings", "0.2.0", true)]);
        manifest(held.path(), 2);
        let catalog = Catalog::at(held.path(), older_os.system().cloned());
        let row = catalog.find("so.tilo.settings").unwrap().unwrap();
        assert!(row.system);

        // Removing the update leaves the image's copy, on either OS.
        std::fs::write(user.path().join("registry.toml"), "").unwrap();
        for os in [on_os(&[1]), on_os(&[1, 2])] {
            let row = os.find("so.tilo.settings").unwrap().unwrap();
            assert_eq!((row.version.as_str(), row.system), ("0.1.10", true));
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
