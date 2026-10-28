//! Source-oriented browser inventory shared with the installer's discovery worker.
//!
//! The report carries recognizable product, profile and count metadata only. It
//! never contains an absolute source path, cookie or origin value, saved
//! password, form value, session token or other private identity. Capabilities
//! describe the actual export path: an unsupported class is named here instead
//! of disappearing behind one browser total.

use browser_continuity::sessions::Service;
use serde::Serialize;

/// Current public report schema. Version two adds the per-profile domain
/// selection flags to the existing inclusion flag.
pub const SCHEMA: u32 = 2;

/// Product identity of an installed browser source.
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
    /// Public product label from the consented discovery registry.
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

    /// Legacy storage family read by the reviewed capture path.
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

/// How the browser was installed, so a native and a Flatpak profile of the
/// same product remain distinct sources.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Installation {
    Native,
    Flatpak,
}

/// What the reviewed export path actually does with one data class.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "state", content = "reason", rename_all = "kebab-case")]
pub enum Capability {
    /// A selected domain is carried through the authenticated browser plan.
    Supported,
    /// Some, but not every, source in this class is carried.
    Limited(&'static str),
    /// Counted for the review, but not carried by the browser transfer.
    InventoryOnly,
    /// No export path exists; the reason is a stable presentation key.
    Unsupported(&'static str),
    /// The browser profile has no data of this class.
    Absent,
}

impl Capability {
    /// Whether a selected source of this class reaches the destination.
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
    /// The source exists but could not be counted without claiming completeness.
    Unknown,
    /// Counting is not part of this class's supported behavior.
    Unsupported,
}

impl Measure {
    /// Exact value when one was proven, otherwise `None`.
    #[must_use]
    pub const fn exact(self) -> Option<u64> {
        match self {
            Self::Exact(value) => Some(value),
            Self::Unknown | Self::Unsupported => None,
        }
    }
}

/// Recognized website-session material in one profile.
///
/// A cookie count is website data, not an authenticated account. No entry here
/// establishes that a person is signed in.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ServiceInventory {
    pub service: Service,
    pub cookies: Measure,
    pub origins: Measure,
    pub account_evidence: bool,
}

/// Website data in one profile. `accounts` stays unsupported until identity
/// evidence exists; it is never the cookie or origin count.
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

/// One profile's data classes and the behavior each one has at the destination.
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

/// Bounded metadata for one reviewed browser profile.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ProfileInventory {
    /// Opaque source identity derived from the recognized granted profile.
    pub id: String,
    pub browser: BrowserKind,
    pub installation: Installation,
    pub application: String,
    pub profile_label: String,
    /// Whether this profile has a reviewed bookmarks/history export selection.
    /// A profile with only website data remains visible but cannot become an
    /// empty plan entry.
    pub exportable: bool,
    /// Whether the current selection carries this profile. The actual plan is
    /// built from the same value, so an exclusion is never display-only.
    pub included: bool,
    /// Whether the current selection carries this profile's bookmarks. A
    /// profile exclusion clears both selected flags, and a per-profile domain
    /// choice changes only its own flag.
    pub bookmarks_selected: bool,
    /// Whether the current selection carries this profile's history.
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
    /// Construct the empty shape for a reviewed profile. Counts are filled only
    /// by actual read-only inspection.
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

    /// Any class whose source was not completely inspected. A report with this
    /// flag must not be presented as a complete inventory.
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

/// Bounded inventory of every reviewed browser profile. Paths and private
/// values are deliberately absent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Report {
    pub schema: u32,
    pub profiles: Vec<ProfileInventory>,
    pub incomplete: bool,
}

/// Stable product identity from the consented discovery registry. Labels are
/// fixed product metadata, never inferred from a private folder name.
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
