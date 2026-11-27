//! What a device's package cache holds, one row per app, as every view of it reads it: the
//! resource `devices/<device>/packages` reads as the list of them, and
//! `devices/<device>/packages/<app>` as one.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// One app's package in a device's cache: on its way, or installed and ready to open.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageRow {
    /// The app's id.
    pub app: String,
    /// Its version.
    pub version: String,
    /// The signed package root, hex.
    pub root: String,
    /// The publisher key that signed the package, hex; empty for a row written before the cache
    /// recorded it, which matches no offer.
    #[serde(default)]
    pub publisher: String,
    /// Where it stands.
    pub state: RowState,
    /// What its installed, signed manifest says to show a person about it: it describes the app
    /// whether or not any repository is reachable. `None` while it is on its way.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub about: Option<PackageAbout>,
    /// The bytes its installed payload takes; 0 while it is on its way.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub size_bytes: u64,
}

#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde hands a skipped field to it by reference"
)]
const fn is_zero(bytes: &u64) -> bool {
    *bytes == 0
}

/// Where one app's package stands on a device.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum RowState {
    /// Its bytes are on their way, `bytes` of them here so far, of `total` when the place they
    /// come from says: the app cannot open yet.
    Fetching {
        bytes: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        total: Option<u64>,
    },
    /// It is installed: the app opens.
    Installed,
    /// It is installed, and the connection it keeps did not come up, and this device stopped
    /// starting it: `reason` says why, in the app's or its launcher's words. Opening the app,
    /// reinstalling it or installing a runtime it needs starts it again.
    Failed { reason: String },
}

impl PackageRow {
    /// The progress of its fetch, `(bytes, total)`, while it is on its way.
    #[must_use]
    pub const fn fetching(&self) -> Option<(u64, Option<u64>)> {
        match &self.state {
            RowState::Fetching { bytes, total } => Some((*bytes, *total)),
            RowState::Installed | RowState::Failed { .. } => None,
        }
    }

    /// Whether the app opens from this device.
    #[must_use]
    pub const fn installed(&self) -> bool {
        !matches!(self.state, RowState::Fetching { .. })
    }

    /// Why the connection this app keeps did not come up here, when it did not and this device
    /// stopped starting it.
    pub fn failed(&self) -> Option<&str> {
        match &self.state {
            RowState::Failed { reason } => Some(reason),
            RowState::Fetching { .. } | RowState::Installed => None,
        }
    }
}

/// The manifest's `[package]` display metadata and declared capabilities, as an installed
/// package's row carries them.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageAbout {
    /// `[package].name`, or a name derived from the id when it is unset.
    pub name: String,
    /// `[package.names]`: the name in other languages, by BCP 47 tag.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub names: BTreeMap<String, String>,
    /// `[package].category`, when declared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    /// `[package.taglines]`: what the app does in one phrase, by BCP 47 tag.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub taglines: BTreeMap<String, String>,
    /// `[capabilities]`, required and optional together: what the app's code may ask the
    /// sandbox for (`network.http`, `files.open`, ...).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
}

impl PackageAbout {
    /// What `manifest` says to show a person about its package.
    #[must_use]
    pub fn of(manifest: &tilo_app_package::Manifest) -> Self {
        let package = &manifest.package;
        Self {
            name: package
                .name
                .clone()
                .filter(|name| !name.trim().is_empty())
                .unwrap_or_else(|| package_store::display_name_for_package_id(&package.id)),
            names: package.names.clone(),
            category: package
                .category
                .clone()
                .filter(|category| !category.trim().is_empty()),
            taglines: package.taglines.clone(),
            capabilities: manifest
                .capabilities
                .required
                .iter()
                .chain(&manifest.capabilities.optional)
                .cloned()
                .collect(),
        }
    }

    /// The name to show in `locale` (BCP 47): its tag, else its language, else [`Self::name`].
    #[must_use]
    pub fn name_for(&self, locale: &str) -> &str {
        tilo_app_package::manifest::text_in(&self.names, locale).unwrap_or(&self.name)
    }

    /// The tag line to show in `locale` (BCP 47): its tag, else its language, else English;
    /// empty when the app declares none.
    #[must_use]
    pub fn tagline_for(&self, locale: &str) -> &str {
        tilo_app_package::manifest::text_in(&self.taglines, locale).unwrap_or_default()
    }
}

/// What the installed package's manifest at `path` says about it; `None` when it cannot be read,
/// which leaves the row describing the app by its id.
fn about(path: &std::path::Path) -> Option<PackageAbout> {
    let text = std::fs::read_to_string(path).ok()?;
    let manifest = tilo_app_package::Manifest::from_toml_str(&text).ok()?;
    Some(PackageAbout::of(&manifest))
}

/// The rows of what `catalog` has installed, with `fetching` (rows of packages on their way) in
/// place of any installed version of the same app, ordered by app. Each app in `failed`, by app
/// and reason, is installed and says why a connection it keeps did not come up.
///
/// # Errors
/// The catalog of what is installed could not be read.
pub fn rows(
    catalog: &package_store::Catalog,
    fetching: impl IntoIterator<Item = PackageRow>,
    failed: impl IntoIterator<Item = (String, String)>,
) -> Result<Vec<PackageRow>, String> {
    let mut rows: BTreeMap<String, PackageRow> = catalog
        .list()
        .map_err(|error| error.to_string())?
        .into_iter()
        .map(|installed| {
            (
                installed.id.clone(),
                PackageRow {
                    about: about(&installed.manifest_path),
                    size_bytes: installed.payload_bytes,
                    app: installed.id,
                    version: installed.version,
                    root: installed.package_id_hex,
                    publisher: installed.publisher_key_hex,
                    state: RowState::Installed,
                },
            )
        })
        .collect();
    for row in fetching {
        rows.insert(row.app.clone(), row);
    }
    for (app, reason) in failed {
        if let Some(row) = rows.get_mut(&app)
            && row.installed()
        {
            row.state = RowState::Failed { reason };
        }
    }
    Ok(rows.into_values().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A manifest's name, names, category, tag lines and capabilities reach its row; a row
    /// without them reads as before.
    #[test]
    fn an_installed_row_says_what_its_manifest_says() {
        let manifest = tilo_app_package::Manifest::from_toml_str(
            r#"
[package]
id = "so.tilo.mail"
name = "Mail"
version = "0.4.0"
kind = "app"
category = "Office"
[package.names]
de = "Post"
[package.taglines]
en = "Read and write email"
[capabilities]
required = ["network.http"]
optional = ["files.open"]
[publisher]
identity = "ed25519:1111111111111111111111111111111111111111111111111111111111111111"
signature = ""
[sandbox]
namespaces = []
seccomp = "tilo-default"
landlock = "strict"
[sandbox.net]
mode = "off"
[entrypoint]
bin = "bin/mail"
"#,
        )
        .unwrap();
        let about = PackageAbout::of(&manifest);
        assert_eq!(about.name_for("de-DE"), "Post");
        assert_eq!(about.name_for("fr"), "Mail");
        assert_eq!(about.tagline_for("en-US"), "Read and write email");
        assert_eq!(about.category.as_deref(), Some("Office"));
        assert_eq!(about.capabilities, ["network.http", "files.open"]);
        let row = PackageRow {
            app: "so.tilo.mail".into(),
            version: "0.4.0".into(),
            root: "ab".repeat(32),
            publisher: "cd".repeat(32),
            state: RowState::Installed,
            about: Some(about),
            size_bytes: 2_400_000,
        };
        let read: PackageRow = serde_json::from_value(serde_json::to_value(&row).unwrap()).unwrap();
        assert_eq!(read, row);
        let bare: PackageRow = serde_json::from_value(serde_json::json!({
            "app": "so.tilo.mail", "version": "0.4.0", "root": "ab", "state": "installed"
        }))
        .unwrap();
        assert!(bare.about.is_none() && bare.size_bytes == 0 && bare.installed());
    }
}
