//! The merged catalogue, and what of it is offered on this device.

use std::collections::HashSet;

use crate::block::BlockList;
use crate::entry::{CatalogueEntry, Source};

/// The identities of the apps this device already has: Tilo package ids,
/// Flatpak application ids and desktop-entry ids, matched in any case. An
/// installed app is never offered again.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Installed {
    ids: HashSet<String>,
}

impl Installed {
    #[must_use]
    pub fn from_ids<I, S>(ids: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        Self {
            ids: ids
                .into_iter()
                .map(|id| canonical(id.as_ref()))
                .filter(|id| !id.is_empty())
                .collect(),
        }
    }

    /// Whether `id` is here. A desktop-entry id (`com.spotify.Client.desktop`)
    /// is the app it launches.
    #[must_use]
    pub fn contains(&self, id: &str) -> bool {
        self.ids.contains(&canonical(id))
    }
}

/// An id as installed apps and catalogue entries are compared: lower case,
/// without a desktop entry's `.desktop`.
fn canonical(id: &str) -> String {
    let id = id.trim();
    id.strip_suffix(".desktop")
        .unwrap_or(id)
        .to_ascii_lowercase()
}

/// Every app a person can get, Tilo's and Flathub's, once each.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Catalogue {
    entries: Vec<CatalogueEntry>,
}

impl Catalogue {
    /// Merge the sources. An id is listed once, the first time it appears;
    /// and a Flathub app with the same name as a Tilo app is left out, as
    /// the Store has always merged them: the Tilo app is the one offered.
    #[must_use]
    pub fn from_sources(tilo: Vec<CatalogueEntry>, flathub: Vec<CatalogueEntry>) -> Self {
        let mut ids = HashSet::new();
        let mut tilo_names = HashSet::new();
        let mut entries = Vec::with_capacity(tilo.len().saturating_add(flathub.len()));
        for entry in tilo {
            if ids.insert(entry.key()) {
                tilo_names.insert(entry.name.trim().to_lowercase());
                entries.push(entry);
            }
        }
        for entry in flathub {
            if !tilo_names.contains(&entry.name.trim().to_lowercase()) && ids.insert(entry.key()) {
                entries.push(entry);
            }
        }
        Self { entries }
    }

    /// Every entry, blocked and installed ones included: the Store's pages
    /// need them to say so.
    #[must_use]
    pub fn entries(&self) -> &[CatalogueEntry] {
        &self.entries
    }

    #[must_use]
    pub fn get(&self, id: &str) -> Option<&CatalogueEntry> {
        let wanted = canonical(id);
        self.entries.iter().find(|entry| entry.key() == wanted)
    }

    /// What is offered here: every entry neither `installed` nor blocked.
    /// This is the list search and the Store's lists show.
    pub fn offered<'a>(
        &'a self,
        installed: &'a Installed,
        blocks: &'a BlockList,
    ) -> impl Iterator<Item = &'a CatalogueEntry> + 'a {
        self.entries
            .iter()
            .filter(move |entry| !installed.contains(&entry.id) && !blocks.is_blocked(&entry.id))
    }

    /// The entries from Tilo registries.
    pub fn tilo(&self) -> impl Iterator<Item = &CatalogueEntry> {
        self.entries
            .iter()
            .filter(|entry| matches!(entry.source, Source::Tilo { .. }))
    }

    /// The entries from Flathub.
    pub fn flathub(&self) -> impl Iterator<Item = &CatalogueEntry> {
        self.entries
            .iter()
            .filter(|entry| matches!(entry.source, Source::Flathub { .. }))
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appstream;
    use crate::block::Blocker;

    fn flathub() -> Vec<CatalogueEntry> {
        let root = std::path::Path::new("/cache");
        appstream::parse(
            appstream::tests::SPOTIFY.as_bytes(),
            &appstream::tests::source(root),
            "en",
        )
        .expect("parses")
    }

    fn tilo(id: &str, name: &str) -> CatalogueEntry {
        crate::tilo::entry(&registry_types::catalogue::CatalogueApp {
            package_id: id.into(),
            display_name: name.into(),
            version: "1.0.0".into(),
            registry: "Tilo".into(),
            trust: "publisher-signed".into(),
            ..Default::default()
        })
    }

    #[test]
    fn installed_and_blocked_apps_are_not_offered_and_tilo_wins_a_shared_name() {
        let catalogue = Catalogue::from_sources(
            vec![
                tilo("so.tilo.music", "Rhythmbox"),
                tilo("so.tilo.sheets", "Sheets"),
            ],
            flathub(),
        );
        assert!(catalogue.get("com.spotify.Client").is_some());
        assert!(
            catalogue.get("org.gnome.Rhythmbox3").is_none(),
            "the Tilo app of the same name is the one offered"
        );
        let nothing = Installed::default();
        let open = BlockList::empty();
        assert_eq!(catalogue.offered(&nothing, &open).count(), 3);

        let installed = Installed::from_ids(["com.spotify.Client.desktop", "SO.TILO.SHEETS"]);
        let offered: Vec<&str> = catalogue
            .offered(&installed, &open)
            .map(|entry| entry.id.as_str())
            .collect();
        assert_eq!(offered, ["so.tilo.music"], "installed apps are not offered");

        let blocked = BlockList::empty().with_workspace_policy("School", ["so.tilo.music"]);
        assert_eq!(catalogue.offered(&installed, &blocked).count(), 0);
        assert_eq!(
            blocked.blockers("so.tilo.music"),
            [&Blocker::Workspace {
                workspace: "School".into()
            }]
        );
    }
}
