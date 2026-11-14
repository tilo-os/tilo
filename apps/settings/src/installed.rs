//! The packages installed on this device, by the name their signed manifests
//! give them.
//!
//! Settings lists apps in several panes — the apps that open a kind of file,
//! the apps notification rules cover, what takes space on disk — and every list
//! names one the same way: the manifest's name, else the words of its package
//! id. The manifests are read here once, so two panes cannot call one package
//! two things.

use std::collections::BTreeMap;

use tilo_app_package::view::ManifestView;

/// An installed package whose signed manifest this device carries.
pub(crate) struct InstalledApp {
    /// The id the store's registry files the package under: what the storage
    /// scan names its tree by.
    pub id: String,
    /// The id the signed manifest calls itself.
    pub package_id: String,
    /// The name the manifest gives it, else the words of its id.
    pub name: String,
    /// The category the manifest files it under.
    pub category: Option<String>,
    pub manifest: ManifestView,
}

/// Every installed package, read from this device's store. A package whose
/// manifest cannot be read is skipped, not a failure of the whole list.
pub(crate) fn installed() -> Vec<InstalledApp> {
    package_store::list_installed()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|entry| {
            let manifest = tilo_app_package::view::read_manifest(&entry.manifest_path).ok()?;
            let package_id = if manifest.package_id.is_empty() {
                entry.id.clone()
            } else {
                manifest.package_id.clone()
            };
            let name = manifest
                .name
                .clone()
                .unwrap_or_else(|| package_store::display_name_for_package_id(&package_id));
            Some(InstalledApp {
                id: entry.id,
                package_id,
                name,
                category: manifest.category.clone(),
                manifest,
            })
        })
        .collect()
}

/// What a list calls an app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AppName {
    pub name: String,
    pub category: Option<String>,
}

/// The installed apps' names, by both ids a tree on disk may be filed under:
/// the registry's and the manifest's.
pub(crate) fn names() -> BTreeMap<String, AppName> {
    let mut names = BTreeMap::new();
    for app in installed() {
        let entry = AppName {
            name: app.name,
            category: app.category,
        };
        names.insert(app.id, entry.clone());
        names.insert(app.package_id, entry);
    }
    names
}
