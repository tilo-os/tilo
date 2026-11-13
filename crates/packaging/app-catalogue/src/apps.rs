//! The installed half of the catalogue: the applications this device can
//! launch, with the signed manifest each was admitted with.
//!
//! Every installed package ([`package_store::Catalog`]) read through the
//! view of its manifest ([`tilo_app_package::view`]), and what each declared
//! it may do on the action door under its own identity. The desktop session,
//! the per-user runtime and app-host launch from this one answer.

use std::path::PathBuf;

use package_store::{Catalog, CatalogError, InstalledPackage};
use tilo_app_id::AppId;

/// One installed application the launcher can start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledApp {
    pub app_id: AppId,
    /// Human-facing name — the manifest's `[package].name`, or derived
    /// from the package id's trailing component when unset.
    pub display_name: String,
    /// The manifest's `[package.names]`: the name in other languages, by
    /// BCP 47 tag. Search matches these alongside `display_name`.
    pub localized_names: std::collections::BTreeMap<String, String>,
    /// The manifest's `[package.keywords]`, every language's together: the
    /// other words the app is known by. Search matches these alongside the
    /// name ("youtube" finds a video app).
    pub keywords: Vec<String>,
    /// The manifest's `[package].category`, when declared; matched by
    /// the Applications search provider.
    pub category: Option<String>,
    pub version: String,
    pub manifest_path: PathBuf,
    pub payload_root: PathBuf,
    /// Whether the package declares a GUI entrypoint; CLI-only packages
    /// have no window, so the applications list hides them.
    pub has_gui: bool,
    /// The manifest's `[providers].ext` — file extensions this package
    /// opens (lowercased, no leading dot). The desktop's only answer to
    /// "which app owns `.odt`": installing an app is what teaches the
    /// desktop its file types.
    pub opens_extensions: Vec<String>,
    /// Concrete normalized MIME claims from `[providers].mime`.
    pub opens_mime_types: Vec<String>,
    pub opens_uri_schemes: Vec<String>,
    /// `[providers].secondary`: another app claiming the same resource is
    /// the default for it.
    pub opens_secondary: bool,
    /// The resource kinds `[providers].resources` says the app opens
    /// (`mail.message`): an address of one opens in this app.
    pub opens_resource_kinds: Vec<String>,
    /// The signed manifest's `[[offers]]`: actions the app offers while it
    /// is not running, which the action catalogue lists for it.
    pub offers: Vec<tilo_app_package::manifest::OfferedAction>,
    /// The signed manifest as it was decoded when the app was read: what a
    /// host answers questions about the app from, without reading the file
    /// again. The fields above are its normalized view.
    pub manifest: tilo_app_package::manifest::Manifest,
}

impl InstalledApp {
    /// The capabilities its signed manifest declares (`[capabilities]`,
    /// required and optional).
    #[must_use]
    pub fn declared_capabilities(&self) -> Vec<String> {
        let capabilities = &self.manifest.capabilities;
        capabilities
            .required
            .iter()
            .chain(&capabilities.optional)
            .cloned()
            .collect()
    }

    /// The application `entry` installs, read through its manifest. `None`
    /// when the manifest is missing (the payload was deleted behind the
    /// registry's back) or unreadable.
    fn from_package(entry: &InstalledPackage) -> Option<Self> {
        // One decode of the signed manifest: the view and the offers are
        // both read from it.
        let source = std::fs::read_to_string(&entry.manifest_path).ok()?;
        let signed = tilo_app_package::manifest::Manifest::from_toml_str(&source).ok()?;
        let offers = signed.offers.clone();
        let opens_resource_kinds = signed.providers.resources.clone();
        let manifest = tilo_app_package::view::ManifestView::try_from(signed.clone()).ok()?;
        let app_id = if manifest.package_id.is_empty() {
            entry.id.clone()
        } else {
            manifest.package_id.clone()
        };
        Some(Self {
            display_name: manifest
                .name
                .clone()
                .filter(|n| !n.trim().is_empty())
                .unwrap_or_else(|| package_store::display_name_for_package_id(&app_id)),
            localized_names: manifest.localized_names.clone(),
            keywords: manifest.keywords.clone(),
            category: manifest.category.clone().filter(|c| !c.trim().is_empty()),
            app_id: AppId::new(app_id),
            version: manifest.version.clone(),
            manifest_path: entry.manifest_path.clone(),
            payload_root: entry.payload_root.clone(),
            // Both conditions must hold: the entrypoint says the package
            // can draw, `kind` says it's something a user opens — the
            // on-screen keyboard satisfies the first but not the second.
            has_gui: manifest.entrypoint.gui_bin().is_some()
                && manifest.kind == tilo_app_package::view::KIND_APP,
            opens_extensions: manifest.opens_ext.clone(),
            opens_mime_types: manifest.opens_mime.clone(),
            opens_uri_schemes: manifest.opens_schemes,
            opens_secondary: manifest.opens_secondary,
            opens_resource_kinds,
            offers,
            manifest: signed,
        })
    }

    /// The name to show in `locale`: [`collab_types::history::localized_name`], else
    /// `display_name`.
    pub fn name_for(&self, locale: &str) -> &str {
        collab_types::history::localized_name(&self.localized_names, locale)
            .unwrap_or(self.display_name.as_str())
    }

    /// The name to show to a person whose interface prefers `locales`, in
    /// that order: the first language the manifest names this app in, else
    /// `display_name`.
    #[must_use]
    pub fn name_in(&self, locales: &[String]) -> &str {
        locales
            .iter()
            .map(|locale| self.name_for(locale))
            .find(|name| !name.trim().is_empty() && *name != self.display_name)
            .unwrap_or(self.display_name.as_str())
    }

    pub fn opens_mime(&self, mime: &str) -> bool {
        desktop_policy::ResourceKind::mime(mime).is_ok_and(|resource| {
            resource
                .mime_name()
                .is_some_and(|mime| self.opens_mime_types.iter().any(|claim| claim == mime))
        })
    }

    /// What the signed manifest's `[actions]` table declares.
    #[must_use]
    pub fn declared_actions(&self) -> DeclaredActions {
        DeclaredActions {
            query: self.manifest.actions.query,
            invoke: self.manifest.actions.invoke,
        }
    }

    /// The resource kinds the signed manifest's `[resources]` table declares
    /// this app's records hold.
    #[must_use]
    pub fn declared_resource_kinds(&self) -> Vec<String> {
        self.manifest.resources.kinds.clone()
    }

    /// Whether the signed manifest's `[capabilities]` lists `capability`
    /// (a `tilo-caps` name such as `open-uri`), required or optional.
    #[must_use]
    pub fn declares_capability(&self, capability: &str) -> bool {
        let declared = &self.manifest.capabilities;
        declared
            .required
            .iter()
            .chain(&declared.optional)
            .any(|name| name == capability)
    }

    /// The icon file the signed manifest's `[package] icon` names, in the
    /// installed payload; `None` for a package without one. Validation keeps
    /// the name a plain relative path inside the payload, so the join cannot
    /// leave it.
    #[must_use]
    pub fn icon_path(&self) -> Option<PathBuf> {
        let icon = self.manifest.package.icon.as_ref()?;
        let path = self.payload_root.join(icon);
        path.is_file().then_some(path)
    }

    /// The ids the signed manifest names as the app's earlier ones (`[package] previously`).
    /// A claim, not an answer: [`earlier_ids`] says which of them are honoured.
    #[must_use]
    pub fn previously(&self) -> Vec<String> {
        self.manifest.package.previously.clone()
    }
}

/// What an application declared it may do on the action door under its own
/// identity. A ceiling, never a grant.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DeclaredActions {
    /// Read the catalogue of actions the person using the app is offered.
    pub query: bool,
    /// Invoke actions other apps publish, as this app.
    pub invoke: bool,
}

/// Every package `catalog` holds, GUI or not, freshly read. A package whose
/// manifest cannot be read is skipped, not a failure of the whole
/// enumeration. An app installed under an id another app took over
/// ([`earlier_ids`]) is that app now and is not listed again.
///
/// # Errors
///
/// Returns the [`CatalogError`] of a registry that cannot be read.
pub fn read(catalog: &Catalog) -> Result<Vec<InstalledApp>, CatalogError> {
    let rows = catalog.list()?;
    let earlier = honoured(catalog, &rows)?;
    Ok(rows
        .iter()
        .filter(|row| earlier.successor(&row.id).is_none())
        .filter_map(InstalledApp::from_package)
        .collect())
}

/// The ids installed apps were published under before (`[package] previously`), as far as the
/// catalogue believes them: an app takes an earlier id's place only from the publisher that
/// signed it. Where the earlier id is still installed, the two must have one publisher key;
/// where it is not, the user's store must have checked that when the app took it over (the same
/// rule as installing it, [`package_store::TakenOver`]), or the app comes with the system image.
///
/// # Errors
///
/// Returns the [`CatalogError`] of a registry that cannot be read.
pub fn earlier_ids(catalog: &Catalog) -> Result<tilo_action_policy::EarlierIds, CatalogError> {
    honoured(catalog, &catalog.list()?)
}

fn honoured(
    catalog: &Catalog,
    rows: &[InstalledPackage],
) -> Result<tilo_action_policy::EarlierIds, CatalogError> {
    let mut out = tilo_action_policy::EarlierIds::new();
    for row in rows {
        let claimed = signed_previously(row);
        if claimed.is_empty() {
            continue;
        }
        let mut believed = Vec::new();
        for old in claimed {
            let ok = match rows.iter().find(|other| other.id == old) {
                Some(installed) => {
                    !row.publisher_key_hex.is_empty()
                        && installed.publisher_key_hex == row.publisher_key_hex
                }
                None => row.system || catalog.taken_over_by(&old)?.as_deref() == Some(&row.id),
            };
            if ok {
                believed.push(old);
            }
        }
        out.insert(&row.id, believed);
    }
    Ok(out)
}

/// What the signed manifest of `row` names as its earlier ids.
fn signed_previously(row: &InstalledPackage) -> Vec<String> {
    std::fs::read_to_string(&row.manifest_path)
        .ok()
        .and_then(|source| tilo_app_package::manifest::Manifest::from_toml_str(&source).ok())
        .map(|manifest| manifest.package.previously)
        .unwrap_or_default()
}

/// The installed app that took the place of the earlier id `old` (see [`earlier_ids`]).
///
/// # Errors
///
/// Returns the [`CatalogError`] of a registry that cannot be read.
pub fn successor_of(catalog: &Catalog, old: &str) -> Result<Option<InstalledApp>, CatalogError> {
    let rows = catalog.list()?;
    let earlier = honoured(catalog, &rows)?;
    let Some(app) = earlier.successor(old) else {
        return Ok(None);
    };
    Ok(rows
        .iter()
        .find(|row| row.id == app)
        .and_then(InstalledApp::from_package)
        .filter(|found| found.app_id == app))
}

/// Read one visible registry row and its manifest. Other installed packages'
/// manifests do not have to be parsed to resolve a service by identity. An id
/// nothing is installed under any more answers the app that took its place
/// ([`successor_of`]): a window, a grant or a reference written under it opens
/// that app.
///
/// # Errors
///
/// Returns a catalogue error when either installed store cannot be read.
pub fn read_one(catalog: &Catalog, app_id: &str) -> Result<Option<InstalledApp>, CatalogError> {
    match catalog.find(app_id)? {
        Some(row) => Ok(InstalledApp::from_package(&row).filter(|app| app.app_id == app_id)),
        None => successor_of(catalog, app_id),
    }
}

/// [`read_one`] over the stores the environment names.
///
/// # Errors
///
/// See [`Catalog::from_env`] and [`read_one`].
pub fn read_one_from_env(app_id: &str) -> Result<Option<InstalledApp>, CatalogError> {
    read_one(&Catalog::from_env()?, app_id)
}

/// Resolve the exact signed package selected by an installed application
/// operation. The logical identity and manifest must agree with that row;
/// a concurrent upgrade must not substitute another package's bytes.
///
/// # Errors
/// Returns a catalogue error when either installed store cannot be read.
pub fn read_exact(
    catalog: &Catalog,
    app_id: &str,
    root: tilo_app_package::PackageId,
) -> Result<Option<InstalledApp>, CatalogError> {
    Ok(catalog.list()?.iter().find_map(|entry| {
        if entry.id != app_id || entry.package_id() != Some(root) {
            return None;
        }
        let app = InstalledApp::from_package(entry)?;
        (app.app_id == app_id).then_some(app)
    }))
}

/// Every installed app's signed `surface.toml`: its package id, the name people call it, and
/// the bytes its signed package ships ([`package_store::integrity::signed_surface`]), for each
/// app that publishes one. What a task's programs may name of the native apps this device has.
#[must_use]
pub fn signed_surfaces(catalog: &Catalog) -> Vec<(AppId, String, Vec<u8>)> {
    let Ok(rows) = catalog.list() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for row in &rows {
        let Some(surface) = package_store::integrity::signed_surface(row) else {
            continue;
        };
        let Some(app) = InstalledApp::from_package(row) else {
            continue;
        };
        out.push((app.app_id, app.display_name, surface));
    }
    out
}

/// [`signed_surfaces`] over the stores the environment names.
///
/// # Errors
///
/// See [`Catalog::from_env`].
pub fn signed_surfaces_from_env() -> Result<Vec<(AppId, String, Vec<u8>)>, CatalogError> {
    Ok(signed_surfaces(&Catalog::from_env()?))
}

/// The signed `surface.toml` of the installed package `app_id`
/// ([`signed_surfaces`] for one app): the bytes its package's signature
/// covers, and with them the person-record kinds it declares, as a host names
/// the surface of the app it is about to run. `None` for an app this device has
/// not installed or one whose package ships no surface.
///
/// # Errors
///
/// Returns the [`CatalogError`] of a registry that cannot be read.
pub fn signed_surface_of(catalog: &Catalog, app_id: &str) -> Result<Option<Vec<u8>>, CatalogError> {
    Ok(catalog
        .find(app_id)?
        .and_then(|row| package_store::integrity::signed_surface(&row)))
}

/// [`signed_surface_of`] over the stores the environment names.
///
/// # Errors
///
/// See [`Catalog::from_env`].
pub fn signed_surface_of_from_env(app_id: &str) -> Result<Option<Vec<u8>>, CatalogError> {
    signed_surface_of(&Catalog::from_env()?, app_id)
}

/// [`read`] over the stores the environment names.
///
/// # Errors
///
/// See [`Catalog::from_env`] and [`read`].
pub fn read_from_env() -> Result<Vec<InstalledApp>, CatalogError> {
    read(&Catalog::from_env()?)
}

/// [`earlier_ids`] of the catalogue the environment names.
///
/// # Errors
///
/// Returns the [`CatalogError`] of a registry that cannot be read.
pub fn earlier_ids_from_env() -> Result<tilo_action_policy::EarlierIds, CatalogError> {
    earlier_ids(&Catalog::from_env()?)
}

/// The directories whose entries change when an app is installed, upgraded
/// or removed in the stores the environment names: each store's root, where
/// its registry is replaced. A reader of [`read_from_env`] that must follow
/// installs watches these.
#[must_use]
pub fn store_directories() -> Vec<PathBuf> {
    [
        package_store::layout::default_user_root(),
        package_store::layout::system_data_dir(),
    ]
    .into_iter()
    .flatten()
    .collect()
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use super::*;

    /// A canonical manifest: `package` is the body of `[package]`, `tail`
    /// the tables after the sandbox.
    fn manifest(package: &str, tail: &str) -> String {
        format!(
            "[package]\n{package}\n[publisher]\nidentity = \"\"\nsignature = \"\"\n\
             [sandbox]\nnamespaces = []\nseccomp = \"tilo-default\"\nlandlock = \"strict\"\n\
             [sandbox.net]\nmode = \"off\"\n{tail}"
        )
    }

    fn install(root: &std::path::Path, id: &str, manifest: &str) -> InstalledPackage {
        let meta = root.join(format!("{id}.pkg"));
        let payload = root.join(id);
        std::fs::create_dir_all(&meta).unwrap();
        std::fs::create_dir_all(&payload).unwrap();
        std::fs::write(meta.join("manifest.toml"), manifest).unwrap();
        InstalledPackage {
            id: id.into(),
            version: "1.0.0".into(),
            package_id_hex: String::new(),
            payload_root: payload,
            manifest_path: meta.join("manifest.toml"),
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

    #[test]
    fn the_catalogue_reads_each_package_through_its_manifest() {
        let tmp = tempfile::tempdir().unwrap();
        let viewer = install(
            tmp.path(),
            "so.tilo.viewer",
            &manifest(
                "id = \"so.tilo.viewer\"\nname = \"Viewer\"\nversion = \"2.0.0\"\nkind = \"app\"",
                "[entrypoint]\nbin = \"bin/viewer\"\ntype = \"gui\"\n\
                 [[offers]]\nresource = \"viewer.shelf\"\naction = \"show-recent\"\n\
                 label = \"Show recent\"\nkind = \"read\"\noperation = \"view\"\n",
            ),
        );
        let tool = install(
            tmp.path(),
            "so.tilo.tool",
            &manifest(
                "id = \"so.tilo.tool\"\nversion = \"1.0.0\"\nkind = \"app\"",
                "[entrypoint]\nbin = \"bin/tool\"\ntype = \"cli\"\n",
            ),
        );
        let mut gone = install(tmp.path(), "so.tilo.gone", "");
        gone.manifest_path = tmp.path().join("missing.toml");
        let user = tmp.path().join("store");
        std::fs::create_dir_all(&user).unwrap();
        let mut rows = String::new();
        for package in [viewer, tool, gone] {
            write!(
                rows,
                "[[entry]]\nid = {:?}\npayload_root = {:?}\nmanifest_path = {:?}\n\n",
                package.id,
                package.payload_root.display().to_string(),
                package.manifest_path.display().to_string(),
            )
            .unwrap();
        }
        std::fs::write(package_store::Layout::new(&user).registry(), rows).unwrap();

        let apps = read(&Catalog::at(&user, None)).unwrap();
        assert_eq!(apps.len(), 2, "{apps:?}");
        let viewer = apps
            .iter()
            .find(|app| app.app_id == "so.tilo.viewer")
            .unwrap();
        assert_eq!(viewer.display_name, "Viewer");
        assert_eq!(viewer.version, "2.0.0");
        assert!(viewer.has_gui);
        let offered = viewer
            .offers
            .iter()
            .map(|offer| (offer.resource.as_str(), offer.action.as_str()))
            .collect::<Vec<_>>();
        assert_eq!(
            offered,
            [("viewer.shelf", "show-recent")],
            "the actions its signed manifest offers while it is closed"
        );
        let tool = apps
            .iter()
            .find(|app| app.app_id == "so.tilo.tool")
            .unwrap();
        assert!(!tool.has_gui);
        assert_eq!(tool.display_name, "Tool");
        assert!(tool.offers.is_empty());
    }

    #[test]
    fn an_earlier_id_answers_the_app_that_took_it_over_from_its_publisher() {
        let tmp = tempfile::tempdir().unwrap();
        let user = tmp.path().join("store");
        std::fs::create_dir_all(&user).unwrap();
        let tasks = install(
            tmp.path(),
            "so.tilo.lang.tasks",
            &manifest(
                "id = \"so.tilo.lang.tasks\"\nversion = \"2.0.0\"\nkind = \"app\"\n\
                 previously = [\"so.tilo.lang.todo\"]",
                "[entrypoint]\nbin = \"bin/tilolang-app\"\ntype = \"gui\"\n",
            ),
        );
        let row = |package: &InstalledPackage, publisher: &str| {
            format!(
                "[[entry]]\nid = {:?}\npayload_root = {:?}\nmanifest_path = {:?}\n\
                 publisher_key_hex = {publisher:?}\n\n",
                package.id,
                package.payload_root.display().to_string(),
                package.manifest_path.display().to_string(),
            )
        };
        let registry = package_store::Layout::new(&user).registry();
        let catalog = Catalog::at(&user, None);

        // A claim alone opens nothing: no store checked who signed the earlier id.
        std::fs::write(&registry, row(&tasks, "aa")).unwrap();
        assert!(read_one(&catalog, "so.tilo.lang.todo").unwrap().is_none());
        assert!(earlier_ids(&catalog).unwrap().is_empty());

        // Taken over at install, from the same publisher: the earlier id is the renamed app.
        std::fs::write(
            &registry,
            format!(
                "{}[[taken-over]]\nid = \"so.tilo.lang.todo\"\nby = \"so.tilo.lang.tasks\"\n",
                row(&tasks, "aa")
            ),
        )
        .unwrap();
        let found = read_one(&catalog, "so.tilo.lang.todo").unwrap().unwrap();
        assert_eq!(found.app_id, "so.tilo.lang.tasks");
        assert_eq!(found.previously(), ["so.tilo.lang.todo"]);
        assert_eq!(
            earlier_ids(&catalog)
                .unwrap()
                .successor("so.tilo.lang.todo"),
            Some("so.tilo.lang.tasks")
        );

        // Both installed: one publisher makes them one app, listed once under the new id;
        // another publisher's claim is not believed.
        let todo = install(
            tmp.path(),
            "so.tilo.lang.todo",
            &manifest(
                "id = \"so.tilo.lang.todo\"\nversion = \"1.0.0\"\nkind = \"app\"",
                "[entrypoint]\nbin = \"bin/tilolang-app\"\ntype = \"gui\"\n",
            ),
        );
        std::fs::write(
            &registry,
            format!("{}{}", row(&todo, "aa"), row(&tasks, "aa")),
        )
        .unwrap();
        let ids: Vec<String> = read(&catalog)
            .unwrap()
            .into_iter()
            .map(|app| app.app_id.to_string())
            .collect();
        assert_eq!(ids, ["so.tilo.lang.tasks"]);
        std::fs::write(
            &registry,
            format!("{}{}", row(&todo, "bb"), row(&tasks, "aa")),
        )
        .unwrap();
        assert_eq!(read(&catalog).unwrap().len(), 2);
        assert!(earlier_ids(&catalog).unwrap().is_empty());
        assert_eq!(
            read_one(&catalog, "so.tilo.lang.todo")
                .unwrap()
                .unwrap()
                .app_id,
            "so.tilo.lang.todo"
        );
    }

    #[test]
    fn the_signed_icon_is_the_file_in_the_installed_payload() {
        let tmp = tempfile::tempdir().unwrap();
        let user = tmp.path().join("store");
        std::fs::create_dir_all(&user).unwrap();
        let notes = install(
            tmp.path(),
            "so.tilo.lang.notes",
            &manifest(
                "id = \"so.tilo.lang.notes\"\nversion = \"0.1.0\"\nkind = \"app\"\n\
                 icon = \"icon.svg\"",
                "[entrypoint]\nbin = \"bin/tilolang-app\"\ntype = \"gui\"\n",
            ),
        );
        let plain = install(
            tmp.path(),
            "so.tilo.plain",
            &manifest(
                "id = \"so.tilo.plain\"\nversion = \"1.0.0\"\nkind = \"app\"",
                "[entrypoint]\nbin = \"bin/plain\"\ntype = \"gui\"\n",
            ),
        );
        let mut rows = String::new();
        for package in [&notes, &plain] {
            write!(
                rows,
                "[[entry]]\nid = {:?}\npayload_root = {:?}\nmanifest_path = {:?}\n\n",
                package.id,
                package.payload_root.display().to_string(),
                package.manifest_path.display().to_string(),
            )
            .unwrap();
        }
        std::fs::write(package_store::Layout::new(&user).registry(), rows).unwrap();
        let catalog = Catalog::at(&user, None);
        let app = read_one(&catalog, "so.tilo.lang.notes").unwrap().unwrap();
        assert_eq!(
            app.icon_path(),
            None,
            "an icon the payload lacks is no icon"
        );
        std::fs::write(notes.payload_root.join("icon.svg"), "<svg/>").unwrap();
        assert_eq!(app.icon_path(), Some(notes.payload_root.join("icon.svg")));
        let plain = read_one(&catalog, "so.tilo.plain").unwrap().unwrap();
        assert_eq!(plain.icon_path(), None);
    }

    #[test]
    fn single_app_lookup_requires_the_canonical_registry_identity() {
        let tmp = tempfile::tempdir().unwrap();
        let user = tmp.path().join("store");
        std::fs::create_dir_all(&user).unwrap();
        let forged = install(
            tmp.path(),
            "so.tilo.other",
            &manifest(
                "id = \"so.tilo.osk\"\nversion = \"9.0.0\"\nkind = \"service\"",
                "[entrypoint]\nbin = \"bin/other\"\ntype = \"gui\"\n",
            ),
        );
        let canonical = install(
            tmp.path(),
            "so.tilo.osk",
            &manifest(
                "id = \"so.tilo.osk\"\nversion = \"1.0.0\"\nkind = \"service\"",
                "[entrypoint]\nbin = \"bin/osk\"\ntype = \"gui\"\n",
            ),
        );
        let row = |package: &InstalledPackage| {
            format!(
                "[[entry]]\nid = {:?}\npayload_root = {:?}\nmanifest_path = {:?}\n",
                package.id,
                package.payload_root.display().to_string(),
                package.manifest_path.display().to_string(),
            )
        };
        let registry = package_store::Layout::new(&user).registry();
        std::fs::write(&registry, row(&forged)).unwrap();
        let catalog = Catalog::at(&user, None);
        assert!(
            read_one(&catalog, "so.tilo.osk").unwrap().is_none(),
            "another row's manifest cannot claim the keyboard identity"
        );

        std::fs::write(&registry, format!("{}{}", row(&forged), row(&canonical))).unwrap();
        let osk = read_one(&catalog, "so.tilo.osk").unwrap().unwrap();
        assert_eq!(osk.app_id, "so.tilo.osk");
        assert_eq!(osk.version, "1.0.0");
        assert_eq!(osk.manifest_path, canonical.manifest_path);

        std::fs::write(
            &canonical.manifest_path,
            manifest(
                "id = \"so.tilo.other\"\nversion = \"1.0.1\"\nkind = \"service\"",
                "[entrypoint]\nbin = \"bin/osk\"\ntype = \"gui\"\n",
            ),
        )
        .unwrap();
        assert!(
            read_one(&catalog, "so.tilo.osk").unwrap().is_none(),
            "a changed keyboard manifest with another identity is unavailable"
        );
    }

    #[test]
    fn exact_launch_selection_preserves_root_and_rejects_manifest_identity_substitution() {
        let tmp = tempfile::tempdir().unwrap();
        let user = tmp.path().join("store");
        std::fs::create_dir_all(&user).unwrap();
        let mut package = install(
            tmp.path(),
            "so.tilo.notes",
            &manifest(
                "id = \"so.tilo.notes\"\nversion = \"2.0.0\"\nkind = \"app\"",
                "[entrypoint]\nbin = \"bin/notes\"\ntype = \"gui\"\n",
            ),
        );
        package.package_id_hex = "11".repeat(32);
        let root = package.package_id().unwrap();
        let row = format!(
            "[[entry]]\nid = {:?}\npackage_id_hex = {:?}\npayload_root = {:?}\nmanifest_path = {:?}\n",
            package.id,
            package.package_id_hex,
            package.payload_root.display().to_string(),
            package.manifest_path.display().to_string()
        );
        std::fs::write(package_store::Layout::new(&user).registry(), row).unwrap();
        let catalog = Catalog::at(&user, None);
        assert_eq!(
            read_exact(&catalog, "so.tilo.notes", root)
                .unwrap()
                .unwrap()
                .version,
            "2.0.0"
        );
        assert!(
            read_exact(
                &catalog,
                "so.tilo.notes",
                "tapp:2222222222222222222222222222222222222222222222222222222222222222"
                    .parse()
                    .unwrap()
            )
            .unwrap()
            .is_none()
        );
        assert!(
            read_exact(&catalog, "so.tilo.browser", root)
                .unwrap()
                .is_none()
        );
        std::fs::write(
            &package.manifest_path,
            manifest(
                "id = \"so.tilo.browser\"\nversion = \"2.0.0\"\nkind = \"app\"",
                "[entrypoint]\nbin = \"bin/browser\"\ntype = \"gui\"\n",
            ),
        )
        .unwrap();
        assert!(
            read_exact(&catalog, "so.tilo.notes", root)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn the_catalogue_uses_the_signed_package_id_not_a_top_level_alias() {
        // A top-level `package_id` is outside the signed schema; listing
        // the package under it would put a stranger's app behind the
        // Store's name.
        let tmp = tempfile::tempdir().unwrap();
        let user = tmp.path().join("store");
        std::fs::create_dir_all(&user).unwrap();
        let confused = format!(
            "package_id = \"so.tilo.store\"\n{}",
            manifest(
                "id = \"com.evil.app\"\nversion = \"1.0.0\"\nkind = \"app\"",
                "[entrypoint]\nbin = \"bin/app\"\n",
            )
        );
        let package = install(tmp.path(), "com.evil.app", &confused);
        let row = format!(
            "[[entry]]\nid = {:?}\npayload_root = {:?}\nmanifest_path = {:?}\n",
            package.id,
            package.payload_root.display().to_string(),
            package.manifest_path.display().to_string()
        );
        std::fs::write(package_store::Layout::new(&user).registry(), row).unwrap();
        let apps = read(&Catalog::at(&user, None)).unwrap();
        let ids: Vec<&str> = apps.iter().map(|app| app.app_id.as_str()).collect();
        assert_eq!(ids, ["com.evil.app"]);
    }

    /// The app the manifest at `toml` describes.
    fn app_of(root: &std::path::Path, toml: &str) -> InstalledApp {
        InstalledApp::from_package(&install(root, "so.tilo.notes", toml))
            .expect("the manifest reads as an app")
    }

    #[test]
    fn an_apps_name_follows_the_interface_language() {
        let tmp = tempfile::tempdir().unwrap();
        let app = app_of(
            tmp.path(),
            &manifest(
                "id = \"so.tilo.notes\"\nname = \"Notes\"\nversion = \"1.0.0\"\nkind = \"app\"\n\
                 [package.names]\nde = \"Notizen\"\nfr = \"Bloc-notes\"",
                "[entrypoint]\nbin = \"bin/notes\"\ntype = \"gui\"\n",
            ),
        );
        let locales = |tags: &[&str]| tags.iter().map(|tag| (*tag).to_owned()).collect::<Vec<_>>();
        assert_eq!(app.name_in(&locales(&["de-DE", "de"])), "Notizen");
        // The first language the manifest names the app in wins, and one it
        // does not name it in falls through to the next.
        assert_eq!(app.name_in(&locales(&["fr-FR", "de"])), "Bloc-notes");
        assert_eq!(app.name_in(&locales(&["es", "de"])), "Notizen");
        assert_eq!(app.name_in(&locales(&["en-US", "en"])), "Notes");
        assert_eq!(app.name_in(&[]), "Notes");
    }

    /// A package that names itself by region (`de-DE`, as a tilolang app's catalogue does) reads
    /// in that language on a machine whose locale says `de_DE.UTF-8`, which the session asks in
    /// as `de_DE` and `de`.
    #[test]
    fn a_name_given_by_region_reads_in_a_posix_locale_of_its_language() {
        let tmp = tempfile::tempdir().unwrap();
        let app = app_of(
            tmp.path(),
            &manifest(
                "id = \"so.tilo.lang.tasks\"\nname = \"Tasks\"\nversion = \"1.0.0\"\nkind = \"app\"\n\
                 [package.names]\nde-DE = \"Aufgaben\"",
                "[entrypoint]\nbin = \"bin/tasks\"\ntype = \"gui\"\n",
            ),
        );
        assert_eq!(
            app.name_in(&["de_DE".to_owned(), "de".to_owned()]),
            "Aufgaben"
        );
        assert_eq!(app.name_in(&["de".to_owned()]), "Aufgaben");
        assert_eq!(app.name_in(&["de-AT".to_owned()]), "Aufgaben");
        assert_eq!(app.name_in(&["en_US".to_owned(), "en".to_owned()]), "Tasks");
    }

    #[test]
    fn declared_resource_kinds_come_from_the_signed_manifest() {
        let tmp = tempfile::tempdir().unwrap();
        let package =
            "id = \"so.tilo.notes\"\nname = \"Notes\"\nversion = \"1.0.0\"\nkind = \"app\"";
        let entrypoint = "[entrypoint]\nbin = \"bin/notes\"\ntype = \"gui\"\n";
        let plain = app_of(tmp.path(), &manifest(package, entrypoint));
        assert!(plain.declared_resource_kinds().is_empty());
        let declared = app_of(
            tmp.path(),
            &manifest(
                package,
                &format!("{entrypoint}[resources]\nkinds = [\"notes.notebook\"]\n"),
            ),
        );
        assert_eq!(declared.declared_resource_kinds(), ["notes.notebook"]);
        // A package whose manifest does not read is not an app at all.
        assert!(
            InstalledApp::from_package(&install(tmp.path(), "so.tilo.gone", "not a manifest"))
                .is_none()
        );
    }
}
