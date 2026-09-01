//! The settings categories shown in the left sidebar.
//!
//! Each [`Category`] carries a vector icon, a user-facing label, a stable slug
//! (used for `test_id`s and CRDT paths), and the [`Section`] it groups
//! under. The sidebar renders sections in [`Section::ALL`] order, and
//! within each section the categories in [`Category::ALL`] order.

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

    /// The section header label.
    pub fn label(self) -> &'static str {
        match self {
            Section::Connectivity => "Connectivity",
            Section::Devices => "Devices",
            Section::System => "System",
            Section::Hubs => "Accounts & Permissions",
            Section::Maintenance => "Maintenance",
        }
    }
}

/// One entry in the settings sidebar.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Category {
    Network,
    Bluetooth,
    DeviceMesh,
    Sound,
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
    "devices",
    "server",
    "host",
    "remote control",
    "relay",
    "copies",
    "files",
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
    "color",
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
            Category::DeviceMesh => Section::Devices,
            Category::Sound
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
            Category::ExternalAccounts | Category::Permissions => Section::Hubs,
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
    pub fn label(self) -> &'static str {
        match self {
            Category::Network => "Network",
            Category::Bluetooth => "Bluetooth",
            Category::DeviceMesh => "Devices & Sync",
            Category::Sound => "Sound",
            Category::Displays => "Displays",
            Category::MouseKeyboard => "Mouse & Keyboard",
            Category::KeyboardShortcuts => "Keyboard Shortcuts",
            Category::TimeDate => "Time & Date",
            Category::Users => "Users",
            Category::Printers => "Printers",
            Category::Power => "Power",
            Category::Appearance => "Appearance",
            Category::Accessibility => "Accessibility",
            Category::Notifications => "Notifications",
            Category::DefaultApps => "Apps & Files",
            Category::Storage => "Storage",
            Category::ExternalAccounts => "External Accounts",
            Category::RegionFormats => "Region & Formats",
            Category::Permissions => "Device Privacy & Security",
            Category::Updates => "Updates",
            Category::About => "About",
        }
    }

    /// A one-line summary shown under the label in the content header.
    pub fn subtitle(self) -> &'static str {
        match self {
            Category::Network => "Wi-Fi, wired connections, and VPN",
            Category::Bluetooth => "Pair and manage Bluetooth devices",
            Category::DeviceMesh => "Copies, remote access, and server roles",
            Category::Sound => "Output, input, volume, and system sounds",
            Category::Displays => "Resolution, scale, arrangement, and brightness",
            Category::MouseKeyboard => "Typing, spelling, input sources, and pointer behaviour",
            Category::KeyboardShortcuts => "System and workspace keyboard shortcuts",
            Category::TimeDate => "Clock, time zone, and how dates display",
            Category::Users => "Accounts on this device and their roles",
            Category::Printers => "Printers, queues, and recent jobs",
            Category::Power => "Battery, power profile, and automatic suspend",
            Category::Appearance => "Theme, contrast, motion, and text size",
            Category::Accessibility => "Vision, pointer, typing, and hearing assists",
            Category::Notifications => "Banners, previews, sounds, and Do Not Disturb",
            Category::DefaultApps => "Your preferred apps for links and file types",
            Category::Storage => "Apps, data, cache, and synced files",
            Category::ExternalAccounts => "Accounts shared with compatible installed apps",
            Category::RegionFormats => "Currency, date, time, and measurement units",
            Category::Permissions => {
                "This device's privacy and security; workspace access is managed per workspace"
            }
            Category::Updates => "Operating-system and firmware updates",
            Category::About => "Device identity and transfer",
        }
    }

    /// Toolkit-owned vector icon rendered before the label in the sidebar.
    /// Keeping category chrome out of private-use font glyphs makes it render
    /// reliably on every supported font stack and at high contrast.
    pub fn icon(self) -> Icon {
        match self {
            Category::Network => Icon::Wifi,
            Category::Bluetooth => Icon::Bluetooth,
            Category::DeviceMesh => Icon::CloudDownload,
            Category::Sound => Icon::Volume2,
            Category::Displays => Icon::Monitor,
            Category::MouseKeyboard => Icon::Mouse,
            Category::KeyboardShortcuts => Icon::Keyboard,
            Category::TimeDate => Icon::Calendar,
            Category::Users => Icon::Users,
            Category::Printers => Icon::Printer,
            Category::Power => Icon::Battery,
            Category::Appearance => Icon::Sparkles,
            Category::Accessibility => Icon::Eye,
            Category::Notifications => Icon::Bell,
            Category::DefaultApps => Icon::LayoutGrid,
            Category::Storage => Icon::HardDrive,
            Category::ExternalAccounts => Icon::CloudDownload,
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
        let q = query.trim().to_ascii_lowercase();
        if q.is_empty() {
            return true;
        }
        if self.label().to_ascii_lowercase().contains(&q)
            || self.subtitle().to_ascii_lowercase().contains(&q)
        {
            return true;
        }
        self.keywords()
            .iter()
            .any(|k| k.contains(&q) || q.contains(*k))
    }

    /// Stable identifier suffix used to compose `test_id`s and CRDT keys.
    pub fn slug(self) -> &'static str {
        match self {
            Category::Network => "network",
            Category::Bluetooth => "bluetooth",
            Category::DeviceMesh => "devices_sync",
            Category::Sound => "sound",
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

    #[test]
    fn all_categories_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for c in Category::ALL {
            assert!(seen.insert(*c), "duplicate category {c:?}");
        }
        assert_eq!(seen.len(), Category::ALL.len());
    }

    #[test]
    fn device_privacy_category_does_not_claim_workspace_authority() {
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
    fn every_category_has_label_subtitle_slug_and_icon() {
        for c in Category::ALL {
            assert!(!c.label().is_empty(), "{c:?} missing label");
            assert!(!c.subtitle().is_empty(), "{c:?} missing subtitle");
            assert!(!c.slug().is_empty(), "{c:?} missing slug");
            let _ = c.icon();
        }
    }

    #[test]
    fn slugs_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for c in Category::ALL {
            assert!(seen.insert(c.slug()), "duplicate slug {}", c.slug());
        }
    }

    #[test]
    fn every_category_belongs_to_a_listed_section() {
        for c in Category::ALL {
            assert!(
                Section::ALL.contains(&c.section()),
                "{c:?} section not in Section::ALL"
            );
        }
    }

    #[test]
    fn sections_partition_all_categories() {
        // Every category appears exactly once when we walk section by section.
        let mut count = 0;
        for s in Section::ALL {
            count += Category::in_section(*s).count();
        }
        assert_eq!(count, Category::ALL.len());
    }

    #[test]
    fn empty_or_whitespace_query_matches_everything() {
        for c in Category::ALL {
            assert!(c.matches(""), "{c:?} should match an empty query");
            assert!(c.matches("   "), "{c:?} should match a whitespace query");
        }
    }

    #[test]
    fn matches_by_label_subtitle_and_keyword() {
        // Label substring.
        assert!(Category::Network.matches("net"));
        // Case-insensitive.
        assert!(Category::Bluetooth.matches("BLUE"));
        // Subtitle substring ("Pair and manage Bluetooth devices").
        assert!(Category::Bluetooth.matches("pair"));
        // Keyword-only terms land on the owning pane.
        assert!(
            Category::Displays.matches("night light"),
            "night light → Displays"
        );
        assert!(Category::Network.matches("vpn"), "vpn → Network");
        assert!(Category::Power.matches("battery"), "battery → Power");
        assert!(
            Category::Appearance.matches("dark mode"),
            "dark mode → Appearance"
        );
        assert!(
            Category::ExternalAccounts.matches("imap"),
            "imap → External Accounts"
        );
        assert!(
            Category::KeyboardShortcuts.matches("hotkeys"),
            "hotkeys → Keyboard Shortcuts"
        );
    }

    #[test]
    fn non_matching_query_is_rejected() {
        assert!(!Category::Network.matches("battery"));
        assert!(!Category::About.matches("wifi"));
        assert!(!Category::Sound.matches("zzzznope"));
    }

    #[test]
    fn every_category_has_nonempty_keywords() {
        for c in Category::ALL {
            assert!(!c.keywords().is_empty(), "{c:?} must carry search keywords");
        }
    }
}
