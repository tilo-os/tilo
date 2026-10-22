//! The settings categories shown in the left sidebar.
//!
//! Each [`Category`] carries a vector icon, a user-facing label, a stable slug
//! (used for `test_id`s and CRDT paths), and the [`Section`] it groups
//! under. The sidebar renders sections in [`Section::ALL`] order, and
//! within each section the categories in [`Category::ALL`] order.

use crate::i18n::t;
use ui::icons::Icon;

/// A labelled group of categories in the sidebar. Sections give the long
/// category list a scannable structure rather than one flat run.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Section {
    /// Network, Bluetooth, Devices & sync: getting online and connected.
    Connectivity,
    /// Displays, sound, the input devices, printers and power: the hardware
    /// in front of you.
    Devices,
    /// Appearance, accessibility, notifications, apps, language and time:
    /// how the desktop behaves for you.
    Personal,
    /// The people, accounts and models other apps rely on, and the device's
    /// privacy. Workspace roles live with the workspace that owns them.
    Hubs,
    /// Storage, updates and the device's identity.
    Maintenance,
}

impl Section {
    /// Render order, top to bottom.
    pub const ALL: &'static [Section] = &[
        Section::Connectivity,
        Section::Personal,
        Section::Devices,
        Section::Hubs,
        Section::Maintenance,
    ];

    /// Stable identifier of the section in the navigation list.
    pub fn slug(self) -> &'static str {
        match self {
            Section::Connectivity => "connectivity",
            Section::Devices => "devices",
            Section::Personal => "personal",
            Section::Hubs => "accounts-permissions",
            Section::Maintenance => "maintenance",
        }
    }

    /// The section header label.
    pub fn label(self) -> String {
        t(match self {
            Section::Connectivity => "settings-section-connectivity",
            Section::Devices => "settings-section-devices",
            Section::Personal => "settings-section-personal",
            Section::Hubs => "settings-section-accounts-permissions",
            Section::Maintenance => "settings-section-system",
        })
    }
}

/// One entry in the settings sidebar.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Category {
    Network,
    Bluetooth,
    DeviceMesh,
    Sound,
    Speakers,
    Displays,
    Keyboard,
    Pointing,
    Spelling,
    KeyboardShortcuts,
    TimeDate,
    Users,
    Printers,
    Power,
    Appearance,
    Accessibility,
    Notifications,
    DefaultApps,
    Storage,
    ExternalAccounts,
    AgentModels,
    RegionFormats,
    Permissions,
    Updates,
    About,
}

impl Category {
    /// Canonical ordering used by the sidebar, grouped by [`Section`]: the
    /// index's page order ([`settings_store::index::PAGES`]).
    pub const ALL: &'static [Category] = &[
        // Connectivity
        Category::Network,
        Category::Bluetooth,
        Category::DeviceMesh,
        // Personal
        Category::Appearance,
        Category::Accessibility,
        Category::Notifications,
        Category::DefaultApps,
        Category::Spelling,
        Category::RegionFormats,
        Category::TimeDate,
        // Devices
        Category::Displays,
        Category::Sound,
        Category::Speakers,
        Category::Keyboard,
        Category::Pointing,
        Category::KeyboardShortcuts,
        Category::Printers,
        Category::Power,
        // Hubs
        Category::Users,
        Category::ExternalAccounts,
        Category::AgentModels,
        Category::Permissions,
        // Maintenance
        Category::Storage,
        Category::Updates,
        Category::About,
    ];

    /// The category shown when Settings first opens and no link names one.
    pub const DEFAULT: Category = Category::Network;

    /// Which section this category groups under.
    pub fn section(self) -> Section {
        match self {
            Category::Network | Category::Bluetooth | Category::DeviceMesh => Section::Connectivity,
            Category::Displays
            | Category::Sound
            | Category::Speakers
            | Category::Keyboard
            | Category::Pointing
            | Category::KeyboardShortcuts
            | Category::Printers
            | Category::Power => Section::Devices,
            Category::Appearance
            | Category::Accessibility
            | Category::Notifications
            | Category::DefaultApps
            | Category::Spelling
            | Category::RegionFormats
            | Category::TimeDate => Section::Personal,
            Category::Users
            | Category::ExternalAccounts
            | Category::AgentModels
            | Category::Permissions => Section::Hubs,
            Category::Storage | Category::Updates | Category::About => Section::Maintenance,
        }
    }

    /// Whether the page is about the workspace's identity rather than this
    /// device: who you are here (Users), the devices that identity trusts
    /// (Devices & sync) and the speakers made of them (Speakers). Until the
    /// workspace has an identity selected such a page asks for one
    /// ([`ui::workspace_identity::required_state`]); every other page works
    /// without it.
    pub fn needs_workspace_identity(self) -> bool {
        matches!(
            self,
            Category::Users | Category::DeviceMesh | Category::Speakers
        )
    }

    /// The categories belonging to a section, in sidebar order.
    pub fn in_section(section: Section) -> impl Iterator<Item = Category> {
        Category::ALL
            .iter()
            .copied()
            .filter(move |c| c.section() == section)
    }

    /// This category's page in the index every search reads.
    pub fn page(self) -> settings_store::index::Page {
        settings_store::index::page(self.slug())
            .unwrap_or_else(|| settings_store::index::Page { slug: self.slug() })
    }

    /// User-facing label, rendered in the sidebar and reused as the pane
    /// header.
    pub fn label(self) -> String {
        t(&self.page().title_key())
    }

    /// A one-line summary of what the page holds.
    pub fn subtitle(self) -> String {
        t(&self.page().subtitle_key())
    }

    /// Toolkit-owned vector icon rendered before the label in the sidebar.
    /// Keeping category chrome out of private-use font glyphs makes it render
    /// reliably on every supported font stack and at high contrast.
    pub fn icon(self) -> Icon {
        match self {
            Category::Network => Icon::Wifi,
            Category::Bluetooth => Icon::Bluetooth,
            Category::DeviceMesh => Icon::Cloud,
            Category::ExternalAccounts => Icon::Mail,
            Category::AgentModels => Icon::Sparkles,
            Category::Appearance => Icon::Palette,
            Category::Sound => Icon::Volume2,
            Category::Speakers => Icon::Speaker,
            Category::Displays => Icon::Monitor,
            Category::Keyboard => Icon::Keyboard,
            Category::KeyboardShortcuts => Icon::Command,
            Category::Pointing => Icon::Mouse,
            Category::Spelling => Icon::Type,
            Category::TimeDate => Icon::Clock,
            Category::Users => Icon::Users,
            Category::Printers => Icon::Printer,
            Category::Power => Icon::Battery,
            Category::Accessibility => Icon::Accessibility,
            Category::Notifications => Icon::Bell,
            Category::DefaultApps => Icon::LayoutGrid,
            Category::Storage => Icon::HardDrive,
            Category::RegionFormats => Icon::Globe,
            Category::Permissions => Icon::Shield,
            Category::Updates => Icon::Refresh,
            Category::About => Icon::Info,
        }
    }

    /// What else this category is found by, beyond its label: each setting
    /// on its page by its name in the desktop's language and its words in
    /// every language, then the words people use for the page itself
    /// ("wlan" and "wifi" both find Network), from the index the desktop
    /// palette reads too.
    pub fn search_terms(self) -> Vec<ui::widget::NavSearchTerm> {
        use settings_store::index::{names_in_every_locale, words_in_every_locale};
        let page = self.page();
        let mut terms: Vec<ui::widget::NavSearchTerm> = page
            .settings()
            .map(|setting| {
                ui::widget::NavSearchTerm::named(t(&setting.name_key()))
                    .also(names_in_every_locale(&setting.name_key()))
                    .also(words_in_every_locale(&setting.words_key()))
            })
            .collect();
        terms.push(
            ui::widget::NavSearchTerm::words(names_in_every_locale(&page.title_key()))
                .also(words_in_every_locale(&page.words_key()))
                .also([self.subtitle()]),
        );
        terms
    }

    /// Every word of [`Self::search_terms`], for a search entry's text.
    pub fn search_words(self) -> Vec<String> {
        self.search_terms()
            .into_iter()
            .flat_map(|term| term.name.into_iter().chain(term.words))
            .collect()
    }

    /// Whether this category matches a search query (case-insensitive): a
    /// substring of its label or of one of its [`Self::search_terms`]. An
    /// all-whitespace query matches everything (the caller treats that as
    /// "no search").
    pub fn matches(self, query: &str) -> bool {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return true;
        }
        self.label().to_lowercase().contains(&q)
            || self
                .search_words()
                .iter()
                .any(|term| term.to_lowercase().contains(&q))
    }

    /// Stable identifier suffix used to compose `test_id`s and CRDT keys.
    /// The category whose [`Self::slug`] is `slug`.
    pub fn from_slug(slug: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|category| category.slug() == slug)
    }

    pub fn slug(self) -> &'static str {
        match self {
            Category::Network => "network",
            Category::Bluetooth => "bluetooth",
            Category::DeviceMesh => "devices_sync",
            Category::Sound => "sound",
            Category::Speakers => "speakers",
            Category::Displays => "displays",
            Category::Keyboard => "keyboard",
            Category::Pointing => "pointing",
            Category::Spelling => "spelling",
            Category::KeyboardShortcuts => "keyboard_shortcuts",
            Category::TimeDate => "time_date",
            Category::Users => "users",
            Category::Printers => "printers",
            Category::Power => "power",
            Category::Appearance => "appearance",
            Category::Accessibility => "accessibility",
            Category::Notifications => "notifications",
            Category::DefaultApps => "default-apps",
            Category::Storage => "storage",
            Category::ExternalAccounts => "external_accounts",
            Category::AgentModels => "agent_models",
            Category::RegionFormats => "region_formats",
            Category::Permissions => "permissions",
            Category::Updates => "updates",
            Category::About => "about",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every category is unique, fully described, keyworded and sits in exactly
    /// one listed section; slugs are unique too.
    #[test]
    fn category_catalogue_is_complete_unique_and_partitioned() {
        let mut categories = std::collections::HashSet::new();
        let mut slugs = std::collections::HashSet::new();
        for c in Category::ALL {
            assert!(categories.insert(*c), "duplicate category {c:?}");
            assert!(!c.label().is_empty(), "{c:?} missing label");
            assert!(!c.subtitle().is_empty(), "{c:?} missing subtitle");
            assert!(!c.slug().is_empty(), "{c:?} missing slug");
            let _ = c.icon();
            assert!(slugs.insert(c.slug()), "duplicate slug {}", c.slug());
            assert!(
                Section::ALL.contains(&c.section()),
                "{c:?} section not in Section::ALL"
            );
            assert!(
                !c.search_terms().is_empty(),
                "{c:?} must carry search terms"
            );
        }
        assert_eq!(categories.len(), Category::ALL.len());

        let count: usize = Section::ALL
            .iter()
            .map(|s| Category::in_section(*s).count())
            .sum();
        assert_eq!(count, Category::ALL.len());

        // The device privacy destination does not claim workspace authority.
        assert_eq!(
            Category::Permissions.label(),
            "Device privacy & security",
            "the navigation destination names its system-wide scope"
        );
        assert!(
            Category::Permissions.subtitle().contains("per workspace"),
            "the destination explains that workspace access belongs to each workspace"
        );
    }

    #[test]
    fn category_queries_match_label_subtitle_and_keyword() {
        for c in Category::ALL {
            assert!(c.matches(""), "{c:?} should match an empty query");
            assert!(c.matches("   "), "{c:?} should match a whitespace query");
        }
        // (category, query, should match, why)
        let cases = [
            (Category::Network, "net", true, "label substring"),
            (Category::Bluetooth, "BLUE", true, "case-insensitive"),
            (Category::Bluetooth, "pair", true, "subtitle substring"),
            // Keyword-only terms land on the owning pane.
            (
                Category::Displays,
                "night light",
                true,
                "night light → Displays",
            ),
            (Category::Network, "vpn", true, "vpn → Network"),
            (
                Category::Keyboard,
                "keyboard layout",
                true,
                "keyboard layout → Keyboard",
            ),
            (Category::Keyboard, "qwertz", true, "qwertz → Keyboard"),
            (Category::Keyboard, "language", true, "language → Keyboard"),
            (Category::Power, "battery", true, "battery → Power"),
            (
                Category::Appearance,
                "dark mode",
                true,
                "dark mode → Appearance",
            ),
            (
                Category::ExternalAccounts,
                "imap",
                true,
                "imap → External Accounts",
            ),
            (
                Category::KeyboardShortcuts,
                "hotkeys",
                true,
                "hotkeys → Keyboard Shortcuts",
            ),
            (
                Category::DeviceMesh,
                "file transfer",
                true,
                "file transfer → Devices & Sync",
            ),
            (
                Category::DeviceMesh,
                "typed gamepad",
                true,
                "typed gamepad → Devices & Sync",
            ),
            (Category::Network, "battery", false, "non-matching keyword"),
            (Category::About, "wifi", false, "non-matching keyword"),
            (Category::Sound, "zzzznope", false, "nonsense query"),
        ];
        for (category, query, want, why) in cases {
            assert_eq!(
                category.matches(query),
                want,
                "{category:?} matches {query:?}: {why}"
            );
        }
    }
}
