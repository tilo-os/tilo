//! Parsing and launch-argument discovery for Flatpak reference files.
//!
//! This lives in the unified store rather than its UI so opening a
//! `.flatpakref` and installing from one use the same package backend as normal
//! catalogue actions.

use std::path::{Path, PathBuf};

use crate::{InstalledState, StoreItem, StoreItemVersion, StoreSource, StoreTrust};

/// The install-relevant fields from a `[Flatpak Ref]` keyfile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FlatpakRef {
    pub app_id: String,
    pub title: String,
    pub comment: String,
    pub url: String,
    pub branch: String,
    pub source_path: PathBuf,
}

impl FlatpakRef {
    /// Parse a Flatpak reference. A non-empty `Name` is the one mandatory
    /// field; title and branch receive the same fallbacks as Flatpak tooling.
    pub fn parse(content: &str, source_path: impl Into<PathBuf>) -> Option<Self> {
        let mut in_ref = false;
        let mut app_id = None;
        let mut title = String::new();
        let mut comment = String::new();
        let mut url = String::new();
        let mut branch = String::new();

        for raw in content.lines() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with(['#', ';']) {
                continue;
            }
            if line.starts_with('[') && line.ends_with(']') {
                in_ref = line.eq_ignore_ascii_case("[Flatpak Ref]");
                continue;
            }
            if !in_ref {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let value = value.trim().to_string();
            match key.trim() {
                "Name" if !value.is_empty() => app_id = Some(value),
                "Title" => title = value,
                "Comment" => comment = value,
                "Url" => url = value,
                "Branch" => branch = value,
                _ => {}
            }
        }

        let app_id = app_id?;
        if title.is_empty() {
            title.clone_from(&app_id);
        }
        if branch.is_empty() {
            branch = "stable".to_string();
        }
        Some(Self {
            app_id,
            title,
            comment,
            url,
            branch,
            source_path: source_path.into(),
        })
    }

    /// Read and parse a `.flatpakref` file. `Ok(None)` means the file read
    /// fine but didn't have the required `[Flatpak Ref]` section/`Name` key;
    /// see [`Self::parse`].
    ///
    /// # Errors
    ///
    /// Returns an error if `path` can't be read.
    pub fn from_path(path: &Path) -> std::io::Result<Option<Self>> {
        let content = std::fs::read_to_string(path)?;
        Ok(Self::parse(&content, path))
    }

    /// Present this reference through the regular Store card/action model.
    /// The user still has to press Install, which is the confirmation gate;
    /// merely opening a file never starts a package transaction.
    pub fn into_store_item(self, installed_version: Option<String>) -> StoreItem {
        let installed =
            installed_version.map_or(InstalledState::Not, |version| InstalledState::Installed {
                available: version.clone(),
                version,
                pinned: true,
                user_managed: true,
            });
        StoreItem {
            bot: None,
            awaiting_developer: false,
            id: self.app_id,
            name: self.title,
            description: self.comment,
            tagline: String::new(),
            supported_languages: Vec::new(),
            categories: vec!["Opened file".to_string()],
            icon_url: None,
            screenshot_urls: Vec::new(),
            homepage_url: None,
            source_url: (!self.url.is_empty()).then_some(self.url),
            install_ref_path: Some(self.source_path.to_string_lossy().into_owned()),
            source: StoreSource::Flathub,
            trust: StoreTrust::Flathub,
            versions: vec![StoreItemVersion {
                version: self.branch,
                released_at_unix: 0,
                download_url: None,
                root_hash_hex: None,
                size_bytes: 0,
                runtime: None,
            }],
            publisher_pubkey_hex: None,
            installed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
[Flatpak Ref]\n\
Name=org.gimp.GIMP\n\
Branch=stable\n\
Title=GIMP\n\
Url=https://dl.flathub.org/repo/\n\
Comment=Create images and edit photographs\n";

    /// .flatpakref fields, fallbacks, group/comment handling, conversion and argument scanning.
    #[test]
    fn flatpakref_parsing_and_install_conversion() {
        // Case `parses_install_fields`.
        {
            let parsed = FlatpakRef::parse(SAMPLE, "/tmp/gimp.flatpakref").unwrap();
            assert_eq!(parsed.app_id, "org.gimp.GIMP");
            assert_eq!(parsed.title, "GIMP");
            assert_eq!(parsed.branch, "stable");
            assert_eq!(parsed.comment, "Create images and edit photographs");
            assert_eq!(parsed.url, "https://dl.flathub.org/repo/");
            assert_eq!(parsed.source_path, PathBuf::from("/tmp/gimp.flatpakref"));
        }

        // Case `requires_name_and_applies_fallbacks`.
        {
            assert!(FlatpakRef::parse("[Flatpak Ref]\nTitle=Nameless\n", "x").is_none());
            let parsed = FlatpakRef::parse("[Flatpak Ref]\nName=org.example.App\n", "x").unwrap();
            assert_eq!(parsed.title, "org.example.App");
            assert_eq!(parsed.branch, "stable");
        }

        // Case `ignores_other_groups_and_ini_comments`.
        {
            let parsed = FlatpakRef::parse(
                "Name=wrong\n[Other]\nName=also.wrong\n[Flatpak Ref]\n; note\nName=real.Id\n",
                "x",
            )
            .unwrap();
            assert_eq!(parsed.app_id, "real.Id");
        }

        // Case `conversion_keeps_reference_path_for_confirmed_install`.
        {
            let item = FlatpakRef::parse(SAMPLE, "/tmp/gimp.flatpakref")
                .unwrap()
                .into_store_item(None);
            assert_eq!(item.id, "org.gimp.GIMP");
            assert_eq!(
                item.install_ref_path.as_deref(),
                Some("/tmp/gimp.flatpakref")
            );
            assert_eq!(item.action_kind(true), crate::ActionKind::FlatpakInstall);
        }
    }
}
