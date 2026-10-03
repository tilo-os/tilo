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
    /// Network, Bluetooth — getting the device online and paired.
    Connectivity,
    /// Sound, Displays, Mouse & Keyboard, Printers — hardware.
    Devices,
    /// Time & Date, Users, Region & Formats — the per-user system setup.
    System,
    /// External Accounts and device privacy — the integration and personal
    /// security surfaces other apps depend on. Workspace roles deliberately
    /// live with the workspace that owns them, not in this system app.
    Hubs,
    /// Updates, About — maintenance and identity.
    Maintenance,
}

impl Section {
    /// Render order, top to bottom.
    pub const ALL: &'static [Section] = &[
        Section::Connectivity,
        Section::Devices,
        Section::System,
        Section::Hubs,
        Section::Maintenance,
    ];

    /// Stable identifier of the section in the navigation list.
    pub fn slug(self) -> &'static str {
        match self {
            Section::Connectivity => "connectivity",
            Section::Devices => "devices",
            Section::System => "system",
            Section::Hubs => "accounts-permissions",
            Section::Maintenance => "maintenance",
        }
    }

    /// The section header label.
    pub fn label(self) -> String {
        t(match self {
            Section::Connectivity => "settings-section-connectivity",
            Section::Devices => "settings-section-devices",
            Section::System => "settings-section-system",
            Section::Hubs => "settings-section-accounts-permissions",
            Section::Maintenance => "settings-section-maintenance",
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
    MouseKeyboard,
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

/// Search-keyword tables backing [`Category::keywords`], one per category —
/// pulled out of the match arms as data so that function stays a short
/// dispatch table instead of a few hundred lines of array literals.
const NETWORK_KEYWORDS: &[&str] = &[
    "wifi",
    "wi-fi",
    "wired",
    "ethernet",
    "vpn",
    "wireguard",
    "proxy",
    "internet",
    "connection",
];
const SPEAKERS_KEYWORDS: &[&str] = &[
    "speakers",
    "stereo",
    "stereo pair",
    "surround",
    "5.1",
    "subwoofer",
    "room",
    "multiroom",
    "calibrate",
    "calibration",
    "speaker group",
];
const BLUETOOTH_KEYWORDS: &[&str] = &[
    "pair",
    "pairing",
    "device",
    "headphones",
    "keyboard",
    "mouse",
    "wireless",
];
const DEVICE_MESH_KEYWORDS: &[&str] = &[
    "sync",
    "continuity",
    "devices",
    "server",
    "host",
    "remote control",
    "relay",
    "copies",
    "files",
    "clipboard",
    "file transfer",
    "realtime sessions",
    "wi-fi settings",
    "media handoff",
    "display extension",
    "notification mirroring",
    "gamepad",
    "typed gamepad",
    "handoff",
    "privacy",
];
const SOUND_KEYWORDS: &[&str] = &[
    "audio",
    "volume",
    "mute",
    "microphone",
    "mic",
    "speaker",
    "output",
    "input",
    "device",
    "system sounds",
];
const DISPLAYS_KEYWORDS: &[&str] = &[
    "monitor",
    "screen",
    "resolution",
    "scale",
    "brightness",
    "night light",
    "blue light",
    "arrangement",
    "external",
];
const MOUSE_KEYBOARD_KEYWORDS: &[&str] = &[
    "pointer",
    "trackpad",
    "touchpad",
    "scroll",
    "natural scrolling",
    "tap to click",
    "key repeat",
    "layout",
    "cursor",
    "spell checker",
    "spelling",
    "autocorrect",
    "auto-correct",
];
const TIME_DATE_KEYWORDS: &[&str] = &[
    "clock",
    "timezone",
    "time zone",
    "ntp",
    "24-hour",
    "automatic time",
    "calendar",
];
const USERS_KEYWORDS: &[&str] = &[
    "accounts",
    "people",
    "administrator",
    "admin",
    "login",
    "roles",
];
const PRINTERS_KEYWORDS: &[&str] = &["cups", "print", "queue", "job", "paper", "scanner"];
const APPEARANCE_KEYWORDS: &[&str] = &[
    "theme",
    "dark mode",
    "light mode",
    "contrast",
    "high contrast",
    "reduce motion",
    "text size",
    "font",
    "wallpaper",
    "background picture",
    "color",
    "automatic theme",
    "sunset",
    "sunrise",
];
const ACCESSIBILITY_KEYWORDS: &[&str] = &[
    "a11y",
    "accessibility",
    "universal access",
    "screen reader",
    "orca",
    "on-screen keyboard",
    "osk",
    "cursor size",
    "large cursor",
    "pointer size",
    "sticky keys",
    "slow keys",
    "visual alerts",
    "high contrast",
    "reduce motion",
    "text size",
    "zoom",
];
const NOTIFICATIONS_KEYWORDS: &[&str] = &[
    "notification",
    "notify",
    "banner",
    "alert",
    "do not disturb",
    "dnd",
    "focus",
    "quiet",
    "sounds",
    "lock screen",
    "previews",
    "badge",
];
const DEFAULT_APPS_KEYWORDS: &[&str] = &[
    "default app",
    "open with",
    "file association",
    "handler",
    "browser",
    "mail",
    "pdf",
];
const STORAGE_KEYWORDS: &[&str] = &[
    "disk space",
    "cache",
    "cleanup",
    "app data",
    "synced files",
    "backup",
];
const EXTERNAL_ACCOUNTS_KEYWORDS: &[&str] = &[
    "email",
    "mail",
    "imap",
    "smtp",
    "calendar",
    "caldav",
    "webdav",
    "nextcloud",
    "sync",
    "cloud",
];
const AGENT_MODELS_KEYWORDS: &[&str] = &[
    "agent",
    "ai",
    "model",
    "models",
    "provider",
    "openai",
    "anthropic",
    "api key",
    "vllm",
    "local model",
    "ollama",
    "llm",
    "tool calling",
];
const REGION_FORMATS_KEYWORDS: &[&str] = &[
    "locale",
    "language",
    "currency",
    "date format",
    "time format",
    "units",
    "measurement",
    "metric",
    "imperial",
];
const PERMISSIONS_KEYWORDS: &[&str] = &[
    "roles",
    "access",
    "sandbox",
    "grants",
    "file access",
    "collaboration",
    "security",
    "privacy",
    "encryption",
    "secure boot",
    "firewall",
    "camera",
    "microphone",
];
const UPDATES_KEYWORDS: &[&str] = &[
    "software", "upgrade", "version", "bootc", "firmware", "rollback", "release",
];
const POWER_KEYWORDS: &[&str] = &[
    "battery",
    "charge",
    "energy",
    "power profile",
    "performance",
    "power saver",
    "suspend",
    "sleep",
    "screen blank",
];
const KEYBOARD_SHORTCUTS_KEYWORDS: &[&str] = &[
    "shortcuts",
    "hotkeys",
    "keybindings",
    "keys",
    "combinations",
    "accelerators",
    "workspace",
];
const ABOUT_KEYWORDS: &[&str] = &[
    "device",
    "identity",
    "os",
    "kernel",
    "hostname",
    "system",
    "version",
    "fingerprint",
    "backup",
    "portable backup",
    "encrypted backup",
    "recovery",
];

impl Category {
    /// Canonical ordering used by the sidebar, grouped by [`Section`].
    pub const ALL: &'static [Category] = &[
        // Connectivity
        Category::Network,
        Category::Bluetooth,
        Category::DeviceMesh,
        // Devices
        Category::Sound,
        Category::Speakers,
        Category::Displays,
        Category::MouseKeyboard,
        Category::KeyboardShortcuts,
        Category::Printers,
        // System
        Category::Appearance,
        Category::Accessibility,
        Category::Notifications,
        Category::DefaultApps,
        Category::Storage,
        Category::Power,
        Category::TimeDate,
        Category::Users,
        Category::RegionFormats,
        // Hubs
        Category::ExternalAccounts,
        Category::AgentModels,
        Category::Permissions,
        // Maintenance
        Category::Updates,
        Category::About,
    ];

    /// The first (default) category shown on launch.
    pub const DEFAULT: Category = Category::Network;

    /// Which section this category groups under.
    pub fn section(self) -> Section {
        match self {
            Category::Network | Category::Bluetooth => Section::Connectivity,
            Category::DeviceMesh
            | Category::Sound
            | Category::Speakers
            | Category::Displays
            | Category::MouseKeyboard
            | Category::KeyboardShortcuts
            | Category::Printers => Section::Devices,
            Category::Appearance
            | Category::Accessibility
            | Category::Notifications
            | Category::DefaultApps
            | Category::Storage
            | Category::Power
            | Category::TimeDate
            | Category::Users
            | Category::RegionFormats => Section::System,
            Category::ExternalAccounts | Category::AgentModels | Category::Permissions => {
                Section::Hubs
            }
            Category::Updates | Category::About => Section::Maintenance,
        }
    }

    /// The categories belonging to a section, in sidebar order.
    pub fn in_section(section: Section) -> impl Iterator<Item = Category> {
        Category::ALL
            .iter()
            .copied()
            .filter(move |c| c.section() == section)
    }

    /// User-facing label, rendered in the sidebar and reused as the pane
    /// header.
    pub fn label(self) -> String {
        t(match self {
            Category::Network => "settings-category-network",
            Category::Bluetooth => "settings-category-bluetooth",
            Category::DeviceMesh => "settings-category-devices-sync",
            Category::Sound => "settings-category-sound",
            Category::Speakers => "settings-category-speakers",
            Category::Displays => "settings-category-displays",
            Category::MouseKeyboard => "settings-category-mouse-keyboard",
            Category::KeyboardShortcuts => "settings-category-keyboard-shortcuts",
            Category::TimeDate => "settings-category-time-date",
            Category::Users => "settings-category-users",
            Category::Printers => "settings-category-printers",
            Category::Power => "settings-category-power",
            Category::Appearance => "settings-category-appearance",
            Category::Accessibility => "settings-category-accessibility",
            Category::Notifications => "settings-category-notifications",
            Category::DefaultApps => "settings-category-default-apps",
            Category::Storage => "settings-category-storage",
            Category::ExternalAccounts => "settings-category-external-accounts",
            Category::AgentModels => "settings-category-agent-models",
            Category::RegionFormats => "settings-category-region-formats",
            Category::Permissions => "settings-category-permissions",
            Category::Updates => "settings-category-updates",
            Category::About => "settings-category-about",
        })
    }

    /// A one-line summary shown under the label in the content header.
    pub fn subtitle(self) -> String {
        t(match self {
            Category::Network => "settings-category-network-subtitle",
            Category::Bluetooth => "settings-category-bluetooth-subtitle",
            Category::DeviceMesh => "settings-category-devices-sync-subtitle",
            Category::Sound => "settings-category-sound-subtitle",
            Category::Speakers => "settings-category-speakers-subtitle",
            Category::Displays => "settings-category-displays-subtitle",
            Category::MouseKeyboard => "settings-category-mouse-keyboard-subtitle",
            Category::KeyboardShortcuts => "settings-category-keyboard-shortcuts-subtitle",
            Category::TimeDate => "settings-category-time-date-subtitle",
            Category::Users => "settings-category-users-subtitle",
            Category::Printers => "settings-category-printers-subtitle",
            Category::Power => "settings-category-power-subtitle",
            Category::Appearance => "settings-category-appearance-subtitle",
            Category::Accessibility => "settings-category-accessibility-subtitle",
            Category::Notifications => "settings-category-notifications-subtitle",
            Category::DefaultApps => "settings-category-default-apps-subtitle",
            Category::Storage => "settings-category-storage-subtitle",
            Category::ExternalAccounts => "settings-category-external-accounts-subtitle",
            Category::AgentModels => "settings-category-agent-models-subtitle",
            Category::RegionFormats => "settings-category-region-formats-subtitle",
            Category::Permissions => "settings-category-permissions-subtitle",
            Category::Updates => "settings-category-updates-subtitle",
            Category::About => "settings-category-about-subtitle",
        })
    }

    /// Toolkit-owned vector icon rendered before the label in the sidebar.
    /// Keeping category chrome out of private-use font glyphs makes it render
    /// reliably on every supported font stack and at high contrast.
    pub fn icon(self) -> Icon {
        match self {
            Category::Network => Icon::Wifi,
            Category::Bluetooth => Icon::Bluetooth,
            Category::DeviceMesh | Category::ExternalAccounts => Icon::CloudDownload,
            Category::AgentModels | Category::Appearance => Icon::Sparkles,
            Category::Sound => Icon::Volume2,
            Category::Speakers => Icon::Music,
            Category::Displays => Icon::Monitor,
            Category::MouseKeyboard => Icon::Mouse,
            Category::KeyboardShortcuts => Icon::Keyboard,
            Category::TimeDate => Icon::Calendar,
            Category::Users => Icon::Users,
            Category::Printers => Icon::Printer,
            Category::Power => Icon::Battery,
            Category::Accessibility => Icon::Eye,
            Category::Notifications => Icon::Bell,
            Category::DefaultApps => Icon::LayoutGrid,
            Category::Storage => Icon::HardDrive,
            Category::RegionFormats => Icon::Globe,
            Category::Permissions => Icon::Shield,
            Category::Updates => Icon::Refresh,
            Category::About => Icon::Info,
        }
    }

    /// Search keywords for this category, beyond its label — the names of the
    /// controls and concepts it houses, plus common synonyms. Search matches a
    /// query against the label, subtitle, and these, so typing "night light",
    /// "vpn", "battery", or "wallpaper" lands on the right pane even though
    /// none of those words is the category's own name.
    pub fn keywords(self) -> &'static [&'static str] {
        match self {
            Category::Network => NETWORK_KEYWORDS,
            Category::Bluetooth => BLUETOOTH_KEYWORDS,
            Category::DeviceMesh => DEVICE_MESH_KEYWORDS,
            Category::Sound => SOUND_KEYWORDS,
            Category::Speakers => SPEAKERS_KEYWORDS,
            Category::Displays => DISPLAYS_KEYWORDS,
            Category::MouseKeyboard => MOUSE_KEYBOARD_KEYWORDS,
            Category::TimeDate => TIME_DATE_KEYWORDS,
            Category::Users => USERS_KEYWORDS,
            Category::Printers => PRINTERS_KEYWORDS,
            Category::Appearance => APPEARANCE_KEYWORDS,
            Category::Accessibility => ACCESSIBILITY_KEYWORDS,
            Category::Notifications => NOTIFICATIONS_KEYWORDS,
            Category::DefaultApps => DEFAULT_APPS_KEYWORDS,
            Category::Storage => STORAGE_KEYWORDS,
            Category::ExternalAccounts => EXTERNAL_ACCOUNTS_KEYWORDS,
            Category::AgentModels => AGENT_MODELS_KEYWORDS,
            Category::RegionFormats => REGION_FORMATS_KEYWORDS,
            Category::Permissions => PERMISSIONS_KEYWORDS,
            Category::Updates => UPDATES_KEYWORDS,
            Category::Power => POWER_KEYWORDS,
            Category::KeyboardShortcuts => KEYBOARD_SHORTCUTS_KEYWORDS,
            Category::About => ABOUT_KEYWORDS,
        }
    }

    /// Whether this category matches a search query (case-insensitive). A
    /// match is a substring hit against the label, the subtitle, or any of the
    /// [`Category::keywords`]. An all-whitespace query matches everything (the
    /// caller treats that as "no search").
    pub fn matches(self, query: &str) -> bool {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return true;
        }
        if self.label().to_lowercase().contains(&q) || self.subtitle().to_lowercase().contains(&q) {
            return true;
        }
        // A keyword matches a query it contains, or one that contains it as
        // whole words: "turn on vpn" finds Network, but "wallpaper" must not
        // find Printers through "paper".
        let padded = format!(" {q} ");
        self.keywords()
            .iter()
            .any(|k| k.contains(&q) || padded.contains(&format!(" {k} ")))
    }

    /// The category whose [`Self::slug`] is `slug`.
    pub fn from_slug(slug: &str) -> Option<Category> {
        Category::ALL.iter().copied().find(|c| c.slug() == slug)
    }

    /// Stable identifier suffix used to compose `test_id`s and CRDT keys.
    pub fn slug(self) -> &'static str {
        match self {
            Category::Network => "network",
            Category::Bluetooth => "bluetooth",
            Category::DeviceMesh => "devices_sync",
            Category::Sound => "sound",
            Category::Speakers => "speakers",
            Category::Displays => "displays",
            Category::MouseKeyboard => "mouse_keyboard",
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
            assert!(!c.keywords().is_empty(), "{c:?} must carry search keywords");
        }
        assert_eq!(categories.len(), Category::ALL.len());

        // Every category appears exactly once when we walk section by section.
        let count: usize = Section::ALL
            .iter()
            .map(|s| Category::in_section(*s).count())
            .sum();
        assert_eq!(count, Category::ALL.len());

        // The device privacy destination does not claim workspace authority.
        assert_eq!(
            Category::Permissions.label(),
            "Device Privacy & Security",
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
            // Subtitle "Pair and manage Bluetooth devices".
            (Category::Bluetooth, "pair", true, "subtitle substring"),
            // Keyword-only terms land on the owning pane.
            (
                Category::Displays,
                "night light",
                true,
                "night light → Displays",
            ),
            (Category::Network, "vpn", true, "vpn → Network"),
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
