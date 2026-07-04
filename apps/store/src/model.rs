//! Data model shared across the store.
//!
//! Everything the UI renders is a [`StoreItem`] — one uniform shape whether
//! the item came from a qOS registry or from Flathub. That merge happens in
//! [`crate::sources`]; downstream (widget, filter, search) only ever sees
//! [`StoreItem`].
//!
//! Kept deliberately serde-friendly so tests can round-trip fixtures through
//! JSON without importing the transport crates. The registry-facing types
//! (`RegistryEntry`, `RegistryVersion`) live in `registry-types`; we do
//! **not** re-export them from here because the store's UI needs to unify
//! them with Flathub records anyway, and using our own uniform shape keeps
//! that seam explicit.

use serde::{Deserialize, Serialize};

/// Which upstream feed a [`StoreItem`] came from.
///
/// Encoded in the item so the widget can pick the right action-button
/// label + colour (Add vs Install) without touching the source-of-truth.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum StoreSource {
    /// A qOS registry entry. The tuple carries the human-readable
    /// registry name so the detail sheet can say "from qOS main".
    Qos { registry_name: String },
    /// A Flathub-hosted Flatpak app.
    Flathub,
}

impl StoreSource {
    /// Short label used on filter chips.
    pub fn short_label(&self) -> &'static str {
        match self {
            StoreSource::Qos { .. } => "qOS",
            StoreSource::Flathub => "Flatpak",
        }
    }
}

/// Whether the item is already on the machine, and if so at what
/// version. The store cross-references its item feed against the
/// on-disk install registry when producing this.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum InstalledState {
    /// Not installed.
    Not,
    /// Installed. `version` is what's on-disk; `available` is the
    /// newest version the source knows about (which drives the
    /// "Update available" indicator).
    Installed { version: String, available: String },
}

impl InstalledState {
    /// True if the source's newest version differs from what's installed.
    pub fn update_available(&self) -> bool {
        match self {
            InstalledState::Not => false,
            InstalledState::Installed { version, available } => version != available,
        }
    }
}

/// Which primary action the card's button will invoke.
///
/// The button label + colour derive from this, so the caller can render
/// with a single `match` and stay in step with the underlying dispatcher
/// in [`crate::actions`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionKind {
    /// qOS registry, not installed → **Add**.
    Add,
    /// qOS registry, installed → **Remove**.
    Remove,
    /// qOS registry, installed but older version available → **Update**.
    Update,
    /// Flatpak, not installed → **Install** (spawns `flatpak install`).
    FlatpakInstall,
    /// Flatpak, installed → **Remove** (spawns `flatpak uninstall`).
    FlatpakRemove,
    /// Flatpak but the host has no `flatpak` binary — offer a link to
    /// Flathub's install docs instead.
    NeedsFlatpakHost,
}

impl ActionKind {
    /// Label shown on the card's action button.
    pub fn label(self) -> &'static str {
        match self {
            ActionKind::Add => "Add",
            ActionKind::Remove => "Remove",
            ActionKind::Update => "Update",
            ActionKind::FlatpakInstall => "Install",
            ActionKind::FlatpakRemove => "Remove",
            ActionKind::NeedsFlatpakHost => "Install flatpak first",
        }
    }
}

/// One version the source knows about.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreItemVersion {
    pub version: String,
    /// Unix seconds. `0` means the source didn't publish a release date.
    pub released_at_unix: u64,
    /// Download URL — a `.qpkg` for qOS entries, `None` for Flatpak
    /// (Flatpak installs go through the `flatpak` CLI, not a URL).
    pub download_url: Option<String>,
    /// BLAKE3 hex root hash of the download. Only meaningful for qOS
    /// entries.
    pub root_hash_hex: Option<String>,
    /// Byte size. `0` when unknown.
    pub size_bytes: u64,
}

/// Unified store item: what every card renders from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreItem {
    /// Stable identifier. For qOS this is the reverse-DNS package id
    /// (`dev.qos.notes`). For Flathub it's the app-id
    /// (`org.gnome.Calculator`).
    pub id: String,
    /// Human-readable name shown in the card title.
    pub name: String,
    /// One-line description under the name.
    pub description: String,
    /// Free-form categories used for the derived category chip row.
    pub categories: Vec<String>,
    /// Icon URL, if the source publishes one. The store may leave this
    /// unfetched at first render and just show a placeholder — decoding
    /// remote images is a follow-up.
    pub icon_url: Option<String>,
    /// Project homepage URL, if known.
    pub homepage_url: Option<String>,
    /// Source-code URL, if the item exposes it. qOS entries do; most
    /// Flathub apps don't via the v2 API.
    pub source_url: Option<String>,
    /// Where this came from.
    pub source: StoreSource,
    /// Newest → oldest.
    pub versions: Vec<StoreItemVersion>,
    /// Publisher pubkey (hex) for qOS entries. `None` for Flatpak.
    pub publisher_pubkey_hex: Option<String>,
    /// Cross-referenced install state at merge time.
    pub installed: InstalledState,
}

impl StoreItem {
    /// The version the UI advertises as "latest" — the first entry, or
    /// an empty string when the source didn't ship any versions.
    pub fn latest_version(&self) -> &str {
        self.versions
            .first()
            .map(|v| v.version.as_str())
            .unwrap_or("")
    }

    /// Which primary action the card should offer this frame.
    pub fn action_kind(&self, flatpak_available: bool) -> ActionKind {
        match (&self.source, &self.installed, flatpak_available) {
            (StoreSource::Qos { .. }, InstalledState::Not, _) => ActionKind::Add,
            (StoreSource::Qos { .. }, InstalledState::Installed { .. }, _) => {
                if self.installed.update_available() {
                    ActionKind::Update
                } else {
                    ActionKind::Remove
                }
            }
            (StoreSource::Flathub, _, false) => ActionKind::NeedsFlatpakHost,
            (StoreSource::Flathub, InstalledState::Not, true) => ActionKind::FlatpakInstall,
            (StoreSource::Flathub, InstalledState::Installed { .. }, true) => {
                ActionKind::FlatpakRemove
            }
        }
    }

    /// Case-insensitive substring match against name + description.
    /// Empty needle matches everything (so the search bar can start empty).
    pub fn matches_query(&self, needle: &str) -> bool {
        let n = needle.trim().to_ascii_lowercase();
        if n.is_empty() {
            return true;
        }
        self.name.to_ascii_lowercase().contains(&n)
            || self.description.to_ascii_lowercase().contains(&n)
    }

    /// Fuzzy rank — higher is better. Used to break ties in the item
    /// list when a search is active. A hit in the name outranks a hit
    /// in the description; an exact prefix outranks a mid-string hit.
    pub fn fuzzy_score(&self, needle: &str) -> u32 {
        let n = needle.trim().to_ascii_lowercase();
        if n.is_empty() {
            return 0;
        }
        let mut score = 0u32;
        let name = self.name.to_ascii_lowercase();
        let desc = self.description.to_ascii_lowercase();
        if name.starts_with(&n) {
            score += 100;
        }
        if let Some(idx) = name.find(&n) {
            score += 50u32.saturating_sub(idx as u32);
        }
        if let Some(idx) = desc.find(&n) {
            score += 10u32.saturating_sub((idx / 8) as u32);
        }
        score
    }
}

/// Which source-side filter chip is active.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SourceFilter {
    /// Show everything.
    All,
    /// Only qOS registry entries.
    Qos,
    /// Only Flatpak entries.
    Flatpak,
    /// Only installed items regardless of source.
    Installed,
}

impl SourceFilter {
    pub const ALL: &'static [SourceFilter] = &[
        SourceFilter::All,
        SourceFilter::Qos,
        SourceFilter::Flatpak,
        SourceFilter::Installed,
    ];

    pub fn label(self) -> &'static str {
        match self {
            SourceFilter::All => "All",
            SourceFilter::Qos => "qOS",
            SourceFilter::Flatpak => "Flatpak",
            SourceFilter::Installed => "Installed",
        }
    }

    /// Does an item pass this filter?
    pub fn accepts(self, item: &StoreItem) -> bool {
        match self {
            SourceFilter::All => true,
            SourceFilter::Qos => matches!(item.source, StoreSource::Qos { .. }),
            SourceFilter::Flatpak => matches!(item.source, StoreSource::Flathub),
            SourceFilter::Installed => !matches!(item.installed, InstalledState::Not),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn qos_item(name: &str, installed: InstalledState, latest: &str) -> StoreItem {
        StoreItem {
            id: format!("dev.qos.{}", name.to_ascii_lowercase()),
            name: name.into(),
            description: format!("Some {} app", name),
            categories: vec!["Productivity".into()],
            icon_url: None,
            homepage_url: None,
            source_url: None,
            source: StoreSource::Qos { registry_name: "qOS main".into() },
            versions: vec![StoreItemVersion {
                version: latest.into(),
                released_at_unix: 0,
                download_url: Some("https://x/y.qpkg".into()),
                root_hash_hex: Some("de".repeat(32)),
                size_bytes: 100,
            }],
            publisher_pubkey_hex: Some("aa".repeat(32)),
            installed,
        }
    }

    fn flatpak_item(name: &str, installed: InstalledState) -> StoreItem {
        StoreItem {
            id: format!("org.example.{}", name),
            name: name.into(),
            description: format!("{} on Flathub", name),
            categories: vec!["Games".into()],
            icon_url: None,
            homepage_url: None,
            source_url: None,
            source: StoreSource::Flathub,
            versions: vec![StoreItemVersion {
                version: "1.0".into(),
                released_at_unix: 0,
                download_url: None,
                root_hash_hex: None,
                size_bytes: 0,
            }],
            publisher_pubkey_hex: None,
            installed,
        }
    }

    #[test]
    fn action_kind_reflects_source_and_install_state() {
        let a = qos_item("Notes", InstalledState::Not, "0.1.0");
        assert_eq!(a.action_kind(true), ActionKind::Add);

        let b = qos_item(
            "Notes",
            InstalledState::Installed { version: "0.1.0".into(), available: "0.1.0".into() },
            "0.1.0",
        );
        assert_eq!(b.action_kind(true), ActionKind::Remove);

        let c = qos_item(
            "Notes",
            InstalledState::Installed { version: "0.1.0".into(), available: "0.2.0".into() },
            "0.2.0",
        );
        assert_eq!(c.action_kind(true), ActionKind::Update);

        let d = flatpak_item("Chess", InstalledState::Not);
        assert_eq!(d.action_kind(true), ActionKind::FlatpakInstall);
        assert_eq!(d.action_kind(false), ActionKind::NeedsFlatpakHost);

        let e = flatpak_item(
            "Chess",
            InstalledState::Installed { version: "1.0".into(), available: "1.0".into() },
        );
        assert_eq!(e.action_kind(true), ActionKind::FlatpakRemove);
    }

    #[test]
    fn search_matches_name_and_description_case_insensitive() {
        let n = qos_item("Notes", InstalledState::Not, "0.1.0");
        assert!(n.matches_query(""));
        assert!(n.matches_query("Notes"));
        assert!(n.matches_query("notes"));
        assert!(n.matches_query("some notes app"));
        assert!(!n.matches_query("terminal"));
    }

    #[test]
    fn fuzzy_score_prefers_name_prefix_hits() {
        let mut n = qos_item("Notes", InstalledState::Not, "0.1.0");
        n.description = "great little app".into();
        let prefix = n.fuzzy_score("no");
        let mid = n.fuzzy_score("les");
        let desc = n.fuzzy_score("little");
        assert!(prefix > mid, "prefix ({prefix}) should outrank mid-name ({mid})");
        assert!(prefix > desc, "prefix ({prefix}) should outrank description ({desc})");
    }

    #[test]
    fn source_filter_gates_items() {
        let q = qos_item("A", InstalledState::Not, "1");
        let f = flatpak_item("B", InstalledState::Not);
        let installed_q = qos_item(
            "C",
            InstalledState::Installed { version: "1".into(), available: "1".into() },
            "1",
        );
        assert!(SourceFilter::All.accepts(&q));
        assert!(SourceFilter::All.accepts(&f));
        assert!(SourceFilter::Qos.accepts(&q));
        assert!(!SourceFilter::Qos.accepts(&f));
        assert!(SourceFilter::Flatpak.accepts(&f));
        assert!(!SourceFilter::Flatpak.accepts(&q));
        assert!(SourceFilter::Installed.accepts(&installed_q));
        assert!(!SourceFilter::Installed.accepts(&q));
    }

    #[test]
    fn update_available_only_true_on_version_mismatch() {
        let same = InstalledState::Installed {
            version: "1".into(),
            available: "1".into(),
        };
        assert!(!same.update_available());
        let diff = InstalledState::Installed {
            version: "1".into(),
            available: "2".into(),
        };
        assert!(diff.update_available());
        assert!(!InstalledState::Not.update_available());
    }
}
