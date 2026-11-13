//! Source-oriented browser inventory shared with the installer's discovery worker.
//!
//! The report carries product, profile and count metadata only, never a path, cookie, origin,
//! password, token or form value; an unsupported class is named instead of hiding behind one
//! browser total.

use browser_continuity::sessions::Service;
use serde::Serialize;

/// Version two adds the per-profile domain selection flags.
pub const SCHEMA: u32 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum BrowserKind {
    Chrome,
    Chromium,
    UngoogledChromium,
    Brave,
    Firefox,
    LibreWolf,
}

impl BrowserKind {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Chrome => "Google Chrome",
            Self::Chromium => "Chromium",
            Self::UngoogledChromium => "Ungoogled Chromium",
            Self::Brave => "Brave",
            Self::Firefox => "Firefox",
            Self::LibreWolf => "LibreWolf",
        }
    }

    #[must_use]
    pub const fn provider(self) -> browser_continuity::Provider {
        match self {
            Self::Firefox | Self::LibreWolf => browser_continuity::Provider::Firefox,
            Self::Chrome | Self::Chromium | Self::UngoogledChromium | Self::Brave => {
                browser_continuity::Provider::Chromium
            }
        }
    }
}

/// A native and a Flatpak profile of the same product stay distinct sources.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Installation {
    Native,
    Flatpak,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "state", content = "reason", rename_all = "kebab-case")]
pub enum Capability {
    Supported,
    Limited(&'static str),
    InventoryOnly,
    Unsupported(&'static str),
    Absent,
}

impl Capability {
    #[must_use]
    pub const fn exported(self) -> bool {
        matches!(self, Self::Supported | Self::Limited(_))
    }
}

/// A bounded count, never a fabricated zero for an unread source.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Measure {
    Exact(u64),
    Unknown,
    Unsupported,
}

impl Measure {
    #[must_use]
    pub const fn exact(self) -> Option<u64> {
        match self {
            Self::Exact(value) => Some(value),
            Self::Unknown | Self::Unsupported => None,
        }
    }
}

/// A cookie count is website data, not an authenticated account; nothing here establishes that a
/// person is signed in.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ServiceInventory {
    pub service: Service,
    pub cookies: Measure,
    pub origins: Measure,
    pub account_evidence: bool,
}

/// Accounts stay unsupported until identity evidence exists; this is never the cookie or origin
/// count.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct WebsiteInventory {
    pub origins: Measure,
    pub cookies: Measure,
    pub accounts: Measure,
    pub account_evidence: bool,
    pub services: Vec<ServiceInventory>,
}

impl Default for WebsiteInventory {
    fn default() -> Self {
        Self {
            origins: Measure::Unsupported,
            cookies: Measure::Unsupported,
            accounts: Measure::Unsupported,
            account_evidence: false,
            services: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Capabilities {
    pub bookmarks: Capability,
    pub history: Capability,
    pub passwords: Capability,
    pub extensions: Capability,
    pub settings: Capability,
    pub sessions: Capability,
    pub website_data: Capability,
}

impl Default for Capabilities {
    fn default() -> Self {
        Self {
            bookmarks: Capability::Absent,
            history: Capability::Absent,
            passwords: Capability::Unsupported("browser-passwords-not-exported"),
            extensions: Capability::Unsupported("browser-extensions-not-exported"),
            settings: Capability::Unsupported("browser-settings-not-exported"),
            sessions: Capability::Limited("recognized-service-sessions-only"),
            website_data: Capability::InventoryOnly,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ProfileInventory {
    pub id: String,
    pub browser: BrowserKind,
    pub installation: Installation,
    pub application: String,
    pub profile_label: String,
    /// A profile with only website data remains visible but cannot become an empty plan entry.
    pub exportable: bool,
    /// The plan is built from the same value, so an exclusion is never display-only.
    pub included: bool,
    /// A profile exclusion clears both flags; a per-profile domain choice changes only its own.
    pub bookmarks_selected: bool,
    pub history_selected: bool,
    pub capabilities: Capabilities,
    pub bookmarks: Measure,
    pub history: Measure,
    pub passwords: Measure,
    pub extensions: Measure,
    pub settings: Measure,
    pub websites: WebsiteInventory,
}

impl ProfileInventory {
    /// Counts are filled only by actual read-only inspection.
    #[must_use]
    pub fn new(
        id: String,
        browser: BrowserKind,
        installation: Installation,
        application: String,
        profile_label: String,
    ) -> Self {
        Self {
            id,
            browser,
            installation,
            application,
            profile_label,
            exportable: false,
            included: false,
            bookmarks_selected: false,
            history_selected: false,
            capabilities: Capabilities::default(),
            bookmarks: Measure::Unsupported,
            history: Measure::Unsupported,
            passwords: Measure::Unsupported,
            extensions: Measure::Unsupported,
            settings: Measure::Unsupported,
            websites: WebsiteInventory::default(),
        }
    }

    /// A report with this flag must not be presented as a complete inventory.
    #[must_use]
    pub fn incomplete(&self) -> bool {
        self.bookmarks == Measure::Unknown
            || self.history == Measure::Unknown
            || self.passwords == Measure::Unknown
            || self.extensions == Measure::Unknown
            || self.settings == Measure::Unknown
            || self.websites.origins == Measure::Unknown
            || self.websites.cookies == Measure::Unknown
            || self.websites.accounts == Measure::Unknown
    }
}

/// Paths and private values are deliberately absent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Report {
    pub schema: u32,
    pub profiles: Vec<ProfileInventory>,
    pub incomplete: bool,
}

/// Labels are fixed product metadata, never inferred from a private folder name.
#[must_use]
pub fn kind_for(provider: crate::account_sources::Provider, label: &str) -> Option<BrowserKind> {
    match (provider, label) {
        (crate::account_sources::Provider::Chromium, "Google Chrome") => Some(BrowserKind::Chrome),
        (crate::account_sources::Provider::Chromium, "Chromium") => Some(BrowserKind::Chromium),
        (crate::account_sources::Provider::Chromium, "Ungoogled Chromium") => {
            Some(BrowserKind::UngoogledChromium)
        }
        (crate::account_sources::Provider::Chromium, "Brave") => Some(BrowserKind::Brave),
        (crate::account_sources::Provider::Firefox, "Firefox") => Some(BrowserKind::Firefox),
        (crate::account_sources::Provider::Firefox, "LibreWolf") => Some(BrowserKind::LibreWolf),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cookie_counts_never_become_account_counts_and_unsupported_classes_stay_named() {
        let mut profile = ProfileInventory::new(
            "a".repeat(64),
            BrowserKind::LibreWolf,
            Installation::Flatpak,
            "LibreWolf".into(),
            "LibreWolf · Personal".into(),
        );
        profile.websites.cookies = Measure::Exact(91);
        profile.websites.origins = Measure::Exact(12);
        assert_eq!(profile.websites.accounts, Measure::Unsupported);
        assert!(!profile.websites.account_evidence);
        assert_eq!(
            profile.capabilities.passwords,
            Capability::Unsupported("browser-passwords-not-exported")
        );
        assert!(!profile.capabilities.passwords.exported());
        assert_eq!(
            profile.capabilities.sessions,
            Capability::Limited("recognized-service-sessions-only")
        );
        assert!(profile.capabilities.sessions.exported());
        assert!(!profile.incomplete());
        profile.history = Measure::Unknown;
        assert!(profile.incomplete());
    }

    #[test]
    fn product_identity_is_separate_for_each_named_browser() {
        for (provider, label, expected) in [
            (
                crate::account_sources::Provider::Chromium,
                "Google Chrome",
                BrowserKind::Chrome,
            ),
            (
                crate::account_sources::Provider::Chromium,
                "Chromium",
                BrowserKind::Chromium,
            ),
            (
                crate::account_sources::Provider::Chromium,
                "Ungoogled Chromium",
                BrowserKind::UngoogledChromium,
            ),
            (
                crate::account_sources::Provider::Firefox,
                "Firefox",
                BrowserKind::Firefox,
            ),
            (
                crate::account_sources::Provider::Firefox,
                "LibreWolf",
                BrowserKind::LibreWolf,
            ),
        ] {
            assert_eq!(kind_for(provider, label), Some(expected));
            assert_eq!(expected.label(), label);
        }
        assert_eq!(
            kind_for(crate::account_sources::Provider::Chromium, "Default"),
            None
        );
    }
}
