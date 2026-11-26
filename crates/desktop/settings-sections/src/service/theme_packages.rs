//! The theme packages installed on this device, as Settings' Appearance page offers them: each
//! package whose payload is a theme bundle (`theme.json` and the painter sets it names), with the
//! name its document gives itself.
//!
//! The list is the device's own installed packages ([`package_store::Catalog`]), never a second
//! register that could drift from what a device can actually draw. Applying one is the appearance
//! section's `apply-theme`, which keeps the package's id in the person's setting.

use serde::{Deserialize, Serialize};

/// The resource the installed theme packages are published as.
pub const RESOURCE: &str = "settings/theme-packages";
/// Its kind, as the reading's type is named and a view reads `settings.theme_packages`.
pub const KIND: &str = "settings.theme-packages";

/// One theme package installed on this device.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(default, rename_all = "kebab-case")]
pub struct ThemePackage {
    /// The package id the appearance names it by.
    pub key: String,
    /// Its name, in the words its theme document gives itself.
    pub name: String,
}

/// The theme packages installed on this device, by name.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(default, rename_all = "kebab-case")]
pub struct ThemePackages {
    pub packages: Vec<ThemePackage>,
}

impl ThemePackages {
    /// The theme packages this device has installed now.
    #[must_use]
    pub fn installed() -> Self {
        let Ok(catalog) = package_store::Catalog::from_env() else {
            return Self::default();
        };
        let mut packages: Vec<ThemePackage> = catalog
            .list()
            .unwrap_or_default()
            .into_iter()
            .filter(|installed| {
                installed
                    .payload_root
                    .join(ui::theme_bundle::DOCUMENT)
                    .is_file()
            })
            .map(|installed| ThemePackage {
                name: ui::theme_bundle::document(&installed.payload_root)
                    .map_or_else(|_| fallback_name(&installed.id), |document| document.name),
                key: installed.id,
            })
            .collect();
        packages.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.key.cmp(&b.key)));
        Self { packages }
    }

    /// Whether a package named `key` is installed here.
    #[must_use]
    pub fn has(&self, key: &str) -> bool {
        self.packages.iter().any(|package| package.key == key)
    }
}

/// What a package is called when its theme document does not read.
fn fallback_name(id: &str) -> String {
    package_store::display_name_for_package_id(id)
}

/// The installed theme packages as a declared type.
#[derive(tilo_resource::Row)]
#[row(kind = "settings.theme-packages", label = "Theme packages")]
pub struct ThemePackagesReading;

/// The installed theme packages as a view of another language reads them
/// (`settings.theme_packages`).
#[must_use]
pub fn surface() -> tilo_resource::surface::Surface {
    tilo_resource::surface::reading_surface::<ThemePackages, tilo_resource::NoEffect>(
        KIND,
        "Theme packages",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_device_with_no_theme_package_installed_reads_an_empty_list() {
        let _home = tilo_test_env::TestHome::new().unwrap();
        let installed = ThemePackages::installed();
        assert!(
            installed.packages.is_empty(),
            "a fresh device has none: {installed:?}"
        );
        assert!(!installed.has("so.tilo.theme.bauhaus"));
    }
}
