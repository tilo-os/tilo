//! The applications a session can launch.
//!
//! Every installed package ([`package_store::Catalog`]) read through the
//! view of its manifest ([`tilo_app_package::view`]), and what each declared
//! it may do on the action door under its own identity. The desktop session and the per-user
//! runtime both launch from this one answer.

use std::path::PathBuf;

use package_store::{Catalog, CatalogError, InstalledPackage};

/// One installed application the launcher can start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledApp {
    /// Reverse-DNS package id — the identity `tilo-install` records.
    pub package_id: String,
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
    /// Normalized URI schemes from `[providers].schemes`.
    pub opens_uri_schemes: Vec<String>,
    /// `[providers].secondary`: another app claiming the same resource is
    /// the default for it.
    pub opens_secondary: bool,
    /// The signed manifest's `[[offers]]`: actions the app offers while it
    /// is not running, which the action catalogue lists for it.
    pub offers: Vec<tilo_app_package::manifest::OfferedAction>,
}

impl InstalledApp {
    /// The application `entry` installs, read through its manifest. `None`
    /// when the manifest is missing (the payload was deleted behind the
    /// registry's back) or unreadable.
    fn from_package(entry: &InstalledPackage) -> Option<Self> {
        // One decode of the signed manifest: the view and the offers are
        // both read from it.
        let source = std::fs::read_to_string(&entry.manifest_path).ok()?;
        let signed = tilo_app_package::manifest::Manifest::from_toml_str(&source).ok()?;
        let offers = signed.offers.clone();
        let manifest = tilo_app_package::view::ManifestView::try_from(signed).ok()?;
        let package_id = if manifest.package_id.is_empty() {
            entry.id.clone()
        } else {
            manifest.package_id.clone()
        };
        Some(Self {
            display_name: manifest
                .name
                .clone()
                .filter(|n| !n.trim().is_empty())
                .unwrap_or_else(|| package_store::display_name_for_package_id(&package_id)),
            localized_names: manifest.localized_names.clone(),
            keywords: manifest.keywords.clone(),
            category: manifest.category.clone().filter(|c| !c.trim().is_empty()),
            package_id,
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
            offers,
        })
    }

    /// The name to show in `locale` (BCP 47): its exact tag, else its
    /// language, else `display_name`.
    pub fn name_for(&self, locale: &str) -> &str {
        let language = locale.split(['-', '_']).next().unwrap_or(locale);
        self.localized_names
            .iter()
            .find(|(tag, _)| tag.eq_ignore_ascii_case(locale))
            .or_else(|| {
                self.localized_names
                    .iter()
                    .find(|(tag, _)| tag.eq_ignore_ascii_case(language))
            })
            .map_or(self.display_name.as_str(), |(_, name)| name.as_str())
    }

    pub fn opens_mime(&self, mime: &str) -> bool {
        desktop_policy::ResourceKind::mime(mime).is_ok_and(|resource| {
            resource
                .mime_name()
                .is_some_and(|mime| self.opens_mime_types.iter().any(|claim| claim == mime))
        })
    }

    /// What the signed manifest's `[actions]` table declares; nothing for
    /// an unreadable manifest.
    #[must_use]
    pub fn declared_actions(&self) -> DeclaredActions {
        self.signed_manifest()
            .map(|manifest| DeclaredActions {
                query: manifest.actions.query,
                invoke: manifest.actions.invoke,
            })
            .unwrap_or_default()
    }

    /// The resource kinds the signed manifest's `[resources]` table declares
    /// this app's records hold; none for an unreadable manifest.
    #[must_use]
    pub fn declared_resource_kinds(&self) -> Vec<String> {
        self.signed_manifest()
            .map(|manifest| manifest.resources.kinds)
            .unwrap_or_default()
    }

    /// Whether the signed manifest's `[capabilities]` lists `capability`
    /// (a `tilo-caps` name such as `open-uri`), required or optional; never
    /// for an unreadable manifest.
    #[must_use]
    pub fn declares_capability(&self, capability: &str) -> bool {
        self.signed_manifest().is_some_and(|manifest| {
            let declared = manifest.capabilities;
            declared
                .required
                .iter()
                .chain(&declared.optional)
                .any(|name| name == capability)
        })
    }

    /// The icon file the signed manifest's `[package] icon` names, in the
    /// installed payload; `None` for a package without one or an unreadable
    /// manifest. Validation keeps the name a plain relative path inside the
    /// payload, so the join cannot leave it.
    #[must_use]
    pub fn icon_path(&self) -> Option<PathBuf> {
        let icon = self.signed_manifest()?.package.icon?;
        let path = self.payload_root.join(icon);
        path.is_file().then_some(path)
    }

    fn signed_manifest(&self) -> Option<tilo_app_package::manifest::Manifest> {
        let source = std::fs::read_to_string(&self.manifest_path).ok()?;
        tilo_app_package::manifest::Manifest::from_toml_str(&source).ok()
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
/// enumeration.
///
/// # Errors
///
/// Returns the [`CatalogError`] of a registry that cannot be read.
pub fn read(catalog: &Catalog) -> Result<Vec<InstalledApp>, CatalogError> {
    Ok(catalog
        .list()?
        .iter()
        .filter_map(InstalledApp::from_package)
        .collect())
}

/// Read one visible registry row and its manifest. Other installed packages'
/// manifests do not have to be parsed to resolve a service by identity.
///
/// # Errors
///
/// Returns a catalogue error when either installed store cannot be read.
pub fn read_one(catalog: &Catalog, app_id: &str) -> Result<Option<InstalledApp>, CatalogError> {
    Ok(catalog
        .find(app_id)?
        .as_ref()
        .and_then(InstalledApp::from_package)
        .filter(|app| app.package_id == app_id))
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
        (app.package_id == app_id).then_some(app)
    }))
}

/// [`read`] over the stores the environment names.
///
/// # Errors
///
/// See [`Catalog::from_env`] and [`read`].
pub fn read_from_env() -> Result<Vec<InstalledApp>, CatalogError> {
    read(&Catalog::from_env()?)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
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
            .find(|app| app.package_id == "so.tilo.viewer")
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
            .find(|app| app.package_id == "so.tilo.tool")
            .unwrap();
        assert!(!tool.has_gui);
        assert_eq!(tool.display_name, "Tool");
        assert!(tool.offers.is_empty());
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
        assert_eq!(osk.package_id, "so.tilo.osk");
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
        let ids: Vec<&str> = apps.iter().map(|app| app.package_id.as_str()).collect();
        assert_eq!(ids, ["com.evil.app"]);
    }

    #[test]
    fn declared_resource_kinds_come_from_the_signed_manifest() {
        let tmp = tempfile::tempdir().unwrap();
        let signed = "[package]\nid = \"so.tilo.notes\"\nversion = \"1.0.0\"\nkind = \"app\"\n\
             [publisher]\nidentity = \"\"\nsignature = \"\"\n\
             [sandbox]\nnamespaces = []\nseccomp = \"tilo-default\"\nlandlock = \"strict\"\n\
             [sandbox.net]\nmode = \"off\"\n";
        let app = |manifest: &str| {
            let path = tmp.path().join("manifest.toml");
            std::fs::write(&path, manifest).unwrap();
            InstalledApp {
                package_id: "so.tilo.notes".into(),
                display_name: "Notes".into(),
                localized_names: BTreeMap::default(),
                keywords: Vec::new(),
                category: None,
                version: "1.0.0".into(),
                manifest_path: path,
                payload_root: tmp.path().into(),
                has_gui: true,
                opens_extensions: Vec::new(),
                opens_mime_types: Vec::new(),
                opens_uri_schemes: Vec::new(),
                opens_secondary: false,
                offers: Vec::new(),
            }
        };
        assert!(app(signed).declared_resource_kinds().is_empty());
        let declared = format!("{signed}[resources]\nkinds = [\"notes.notebook\"]\n");
        assert_eq!(app(&declared).declared_resource_kinds(), ["notes.notebook"]);
        assert!(app("not a manifest").declared_resource_kinds().is_empty());
    }
}
