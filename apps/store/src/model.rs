//! Data model shared across the store.
//!
//! Everything the UI renders is a [`StoreItem`] — one uniform shape whether
//! the item came from a Tilo registry or from Flathub. That merge happens in
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
use ui::search::{FieldWeight, ListIndex};

/// Which upstream feed a [`StoreItem`] came from.
///
/// Encoded in the item so the widget can pick the right action-button
/// label + colour (Add vs Install) without touching the source-of-truth.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum StoreSource {
    /// A Tilo registry entry. The tuple carries the human-readable
    /// registry name so the detail sheet can say "from Tilo main".
    Tilo { registry_name: String },
    /// A Flathub-hosted Flatpak app.
    Flathub,
    /// A configured Flatpak repository. Its original scope is retained.
    FlatpakRemote {
        name: String,
        title: String,
        branch: String,
        user_managed: bool,
    },
}

impl StoreSource {
    /// The repository whose metadata is shown, without inventing trust claims.
    pub fn distribution_name(&self) -> &str {
        match self {
            Self::Tilo { registry_name } => registry_name,
            Self::Flathub => "Flathub",
            Self::FlatpakRemote { title, .. } => title,
        }
    }

    /// Short label used on filter chips.
    pub fn short_label(&self) -> &'static str {
        match self {
            StoreSource::Tilo { .. } => "Tilo",
            StoreSource::Flathub | StoreSource::FlatpakRemote { .. } => "Flatpak",
        }
    }
}

/// Provenance claim the Store can substantiate for an item.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum StoreTrust {
    /// Signed local file authorized by this person's existing scoped attestor.
    TrustedAttestor,
    PublisherSigned,
    Flathub,
    /// Metadata from the device’s configured Flatpak repository.
    ConfiguredRemote,
}

impl StoreTrust {
    pub fn label(self) -> String {
        match self {
            Self::TrustedAttestor => ui::i18n!("store-package-local-trust"),
            Self::PublisherSigned => "Publisher + registry verified".to_string(),
            Self::Flathub => "Flathub distribution".to_string(),
            Self::ConfiguredRemote => ui::i18n!("store-remote-trust-short"),
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
    /// Installed / cached. `version` is what's on-disk; `available`
    /// is the newest version the source knows about (which drives
    /// the "Update available" indicator). `pinned` is the user's
    /// "mark installed" flag — a pinned app is kept, ranked first,
    /// and never evicted under size pressure.
    Installed {
        version: String,
        available: String,
        #[serde(default)]
        pinned: bool,
        /// Whether this package can be changed through the app-host
        /// capability. Immutable system RPM packages are launchable but
        /// cannot be pinned or removed from the user store.
        #[serde(default = "default_user_managed")]
        user_managed: bool,
    },
}

fn default_user_managed() -> bool {
    true
}

impl InstalledState {
    /// True if the source offers a newer version than what's installed. A
    /// source that only has an older one (an app the system image ships in
    /// a newer build than the catalogue) offers no update.
    pub fn update_available(&self) -> bool {
        match self {
            InstalledState::Not => false,
            InstalledState::Installed {
                version, available, ..
            } => {
                !available.is_empty() && ui::registry::version::compare(available, version).is_gt()
            }
        }
    }

    /// Whether this app is present (cached / installed) at all.
    pub fn is_installed(&self) -> bool {
        matches!(self, InstalledState::Installed { .. })
    }

    /// Whether the user has pinned ("marked installed") this app.
    pub fn is_pinned(&self) -> bool {
        matches!(self, InstalledState::Installed { pinned: true, .. })
    }
}

/// Which primary action the card's button will invoke.
///
/// The button label + colour derive from this, so the caller can render
/// with a single `match` and stay in step with the underlying dispatcher
/// in [`crate::actions`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionKind {
    /// Tilo registry → **Open**. Fetch-on-demand (if not already
    /// cached) + launch. The Tilo path never shows a separate
    /// "Install" step — opening an app *is* adding it.
    Open,
    /// Install a runtime without launching its host executable.
    RuntimeInstall,
    /// Tilo registry, installed → **Remove**.
    Remove,
    /// Tilo registry, installed but older version available → **Update**.
    Update,
    /// Tilo registry, cached but not pinned → **Keep on device** (pin).
    MarkInstalled,
    /// Tilo registry, pinned → **Kept on device** (click allows cleanup).
    Unmark,
    /// Flatpak, not installed → **Install** (spawns `flatpak install`).
    FlatpakInstall,
    /// Flatpak, installed with a newer catalogue version → **Update**.
    FlatpakUpdate,
    /// Flatpak, installed → **Open**.
    ///
    /// Installing an app and then having no way to start it is not a
    /// store. An installed Flatpak's primary action is the same as
    /// every other installed app's — open it — and removal moves to the
    /// secondary slot where the Tilo side already keeps it.
    ///
    /// It opens **unconfined**: a Flatpak brings its own sandbox and
    /// cannot run inside Tilo's, so the launch goes through
    /// `tilo-launcher run-host`, which says so. See `launcher::host`.
    FlatpakOpen,
    /// Flatpak, installed → **Remove** (spawns `flatpak uninstall`).
    FlatpakRemove,
    /// Flatpak but the host has no `flatpak` binary — offer a link to
    /// Flathub's install docs instead.
    NeedsFlatpakHost,
    /// A directory bot → **Add to a room…**. A bot never installs on the
    /// device; it runs in a room, added through that room's governance.
    AddBot,
}

impl ActionKind {
    /// Label shown on the card's action button.
    pub fn label(self) -> &'static str {
        match self {
            ActionKind::Open | ActionKind::FlatpakOpen => "Open",
            ActionKind::Remove | ActionKind::FlatpakRemove => "Remove",
            ActionKind::Update | ActionKind::FlatpakUpdate => "Update",
            ActionKind::MarkInstalled => "Keep on device",
            ActionKind::Unmark => "Allow cleanup",
            ActionKind::FlatpakInstall | ActionKind::RuntimeInstall => "Install",
            ActionKind::NeedsFlatpakHost => "Install flatpak first",
            ActionKind::AddBot => "Add to this workspace…",
        }
    }

    /// Present-progress verb for the one card action currently running.
    /// This stays coupled to the actual action identity so a Tilo `Open`
    /// (which may fetch before launch) never misleadingly renders as a
    /// Flatpak installation, and vice versa.
    pub fn progress_label(self) -> &'static str {
        match self {
            Self::Open | Self::FlatpakOpen => "Starting",
            Self::Remove | Self::FlatpakRemove => "Removing",
            Self::Update | Self::FlatpakUpdate => "Updating",
            Self::MarkInstalled => "Keeping on device",
            Self::Unmark => "Allowing cleanup",
            Self::FlatpakInstall | Self::RuntimeInstall => "Installing",
            Self::NeedsFlatpakHost => "Preparing Flatpak",
            Self::AddBot => "Adding",
        }
    }

    /// Whether this operation requires the host's Flatpak runtime. These are
    /// never merely view choices: each one eventually asks the host to invoke
    /// Flatpak or launch its independently-sandboxed desktop application.
    pub fn requires_flatpak_host(self) -> bool {
        matches!(
            self,
            Self::FlatpakInstall
                | Self::FlatpakUpdate
                | Self::FlatpakOpen
                | Self::FlatpakRemove
                | Self::NeedsFlatpakHost
        )
    }
}

/// What a directory bot does and needs, from its newest signed version.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotListing {
    pub version: String,
    /// Trigger kinds (`connector`, `record_changed`, …).
    pub triggers: Vec<String>,
    pub apps: Vec<String>,
    pub connectors: Vec<ui::registry::PackageRequirement>,
    /// Whether it may post under other names and avatars.
    pub appearance: bool,
    pub artifact: ui::registry::BotArtifact,
}

/// What the catalogue says about an app beyond its name and versions: who
/// makes it, under which licence, when it last changed, whether its source
/// features it, and what its signed manifest lets it do.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Listing {
    /// Who makes the app, as its source names them; empty when it does not.
    #[serde(default)]
    pub developer: String,
    /// SPDX licence expression; empty when the source does not say.
    #[serde(default)]
    pub license: String,
    /// When the app last changed in its source, Unix seconds; 0 when unknown.
    #[serde(default)]
    pub updated_at_unix: u64,
    /// Its place in its source's featured list, best first; `None` when the
    /// source does not feature it.
    #[serde(default)]
    pub featured_rank: Option<u32>,
    /// The capabilities its signed manifest declares, once it is on this
    /// device (`network.http`, `files.open`, ...).
    #[serde(default)]
    pub capabilities: Vec<String>,
}

/// One version the source knows about.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreItemVersion {
    /// Signed runtime provided/required by this exact package version.
    #[serde(default)]
    pub runtime: Option<String>,
    pub version: String,
    /// Unix seconds. `0` means the source didn't publish a release date.
    pub released_at_unix: u64,
    /// Download URL — a `.tapp` for Tilo entries, `None` for Flatpak
    /// (Flatpak installs go through the `flatpak` CLI, not a URL).
    pub download_url: Option<String>,
    /// BLAKE3 hex root hash of the download. Only meaningful for Tilo
    /// entries.
    pub root_hash_hex: Option<String>,
    /// Byte size. `0` when unknown.
    pub size_bytes: u64,
}

/// Unified store item: what every card renders from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreItem {
    /// Stable identifier. For Tilo this is the reverse-DNS package id
    /// (`so.tilo.notes`). For Flathub it's the app-id
    /// (`org.gnome.Calculator`).
    pub id: String,
    /// Human-readable name shown in the card title.
    pub name: String,
    /// One-line description under the name.
    pub description: String,
    /// Short localized publisher tagline.
    #[serde(default)]
    pub tagline: String,
    /// Canonical language tags supported by the app itself.
    #[serde(default)]
    pub supported_languages: Vec<String>,
    /// Free-form categories used for the derived category chip row.
    pub categories: Vec<String>,
    /// Icon URL, if the source publishes one. Artwork is fetched lazily
    /// through the Store's bounded HTTPS-only image cache; rejected or
    /// unavailable images use deterministic initial artwork.
    pub icon_url: Option<String>,
    /// Source-provided preview artwork, fetched lazily through the bounded
    /// HTTPS-only cache used for icons.
    #[serde(default)]
    pub screenshot_urls: Vec<String>,
    /// Project homepage URL, if known.
    pub homepage_url: Option<String>,
    /// Source-code URL, if the item exposes it. Tilo entries do; most
    /// Flathub apps don't via the v2 API.
    pub source_url: Option<String>,
    /// Original local `.flatpakref` selected through file-open integration.
    ///
    /// Catalogue Flatpaks leave this empty and install by app id from
    /// Flathub. A populated value keeps the file-open flow on the same card
    /// and action path while ensuring Flatpak receives the exact reference
    /// the user chose.
    #[serde(default)]
    pub install_ref_path: Option<String>,
    /// Where this came from.
    pub source: StoreSource,
    /// Source verification retained through the full ingestion/UI pipeline.
    pub trust: StoreTrust,
    /// Newest → oldest.
    pub versions: Vec<StoreItemVersion>,
    /// Publisher pubkey (hex) for Tilo entries. `None` for Flatpak.
    pub publisher_pubkey_hex: Option<String>,
    /// Cross-referenced install state at merge time.
    pub installed: InstalledState,
    /// Set for a directory bot, which has no downloadable app version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bot: Option<BotListing>,
    /// The app publishes builds for this machine, but every one speaks an
    /// older interface version than this system serves: it is waiting for
    /// an update from its developer, and the listing says so
    /// (docs/architecture/compatibility.md).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub awaiting_developer: bool,
    /// Developer, licence, freshness and capabilities, when the source says.
    #[serde(default)]
    pub listing: Listing,
}

impl StoreItem {
    /// The publisher's short pitch, or the source description when an older
    /// catalogue does not provide a tagline. Cards and semantic resources
    /// use the same summary; the details page still shows the full description.
    pub fn summary(&self) -> &str {
        let tagline = self.tagline.trim();
        if tagline.is_empty() {
            self.description.trim()
        } else {
            tagline
        }
    }

    /// The version the UI advertises as "latest" — the first entry, or
    /// an empty string when the source didn't ship any versions.
    pub fn latest_version(&self) -> &str {
        self.versions.first().map_or("", |v| v.version.as_str())
    }

    /// Which primary action the card should offer this frame.
    ///
    /// For Tilo apps the primary is always **Open** — it fetches the
    /// package on demand the first time and launches it every time
    /// (never a distinct "Install" step). **Update** takes its place
    /// when a newer version is available. Flatpak apps keep the
    /// legacy Install / Remove semantics.
    pub fn action_kind(&self, flatpak_available: bool) -> ActionKind {
        if self.bot.is_some() {
            return ActionKind::AddBot;
        }
        if matches!(self.source, StoreSource::Tilo { .. })
            && ui::registry::is_runtime_package_id(&self.id)
        {
            return ActionKind::RuntimeInstall;
        }
        match (&self.source, &self.installed, flatpak_available) {
            (StoreSource::Tilo { .. }, InstalledState::Not, _) => ActionKind::Open,
            (StoreSource::Tilo { .. }, InstalledState::Installed { .. }, _) => {
                if self.installed.update_available() {
                    ActionKind::Update
                } else {
                    ActionKind::Open
                }
            }
            (StoreSource::Flathub | StoreSource::FlatpakRemote { .. }, _, false) => {
                ActionKind::NeedsFlatpakHost
            }
            (
                StoreSource::Flathub | StoreSource::FlatpakRemote { .. },
                InstalledState::Not,
                true,
            ) => ActionKind::FlatpakInstall,
            (
                StoreSource::Flathub | StoreSource::FlatpakRemote { .. },
                InstalledState::Installed { .. },
                true,
            ) if !self.latest_version().is_empty()
                && self.installed.update_available()
                && matches!(
                    self.installed,
                    InstalledState::Installed {
                        user_managed: true,
                        ..
                    }
                ) =>
            {
                ActionKind::FlatpakUpdate
            }
            (
                StoreSource::Flathub | StoreSource::FlatpakRemote { .. },
                InstalledState::Installed { .. },
                true,
            ) => ActionKind::FlatpakOpen,
        }
    }

    /// The optional **pin** ("Keep on this device") a Tilo app offers beside
    /// Open: on an app not downloaded yet it downloads and keeps it without
    /// opening it; on a cached one it keeps it or allows its cleanup. `None`
    /// for Flatpak apps, whose install *is* the pin, and for bots and
    /// runtime components.
    pub fn pin_action(&self) -> Option<ActionKind> {
        if self.bot.is_some()
            || ui::registry::is_runtime_package_id(&self.id)
            || ui::registry::is_language_pack_package_id(&self.id)
        {
            return None;
        }
        match (&self.source, &self.installed) {
            (StoreSource::Tilo { .. }, InstalledState::Not)
                if !self.versions.is_empty() && !self.awaiting_developer =>
            {
                Some(ActionKind::MarkInstalled)
            }
            (
                StoreSource::Tilo { .. },
                InstalledState::Installed {
                    pinned,
                    user_managed,
                    ..
                },
            ) if *user_managed => Some(if *pinned {
                ActionKind::Unmark
            } else {
                ActionKind::MarkInstalled
            }),
            _ => None,
        }
    }

    /// The secondary **Remove** an installed card offers, if any.
    ///
    /// One function rather than a `bool` plus a guess at the caller,
    /// because the two sources remove differently — `tilo-install
    /// uninstall` versus `flatpak uninstall` — and a caller that has to
    /// re-derive which is which is a caller that can get it wrong.
    pub fn remove_action(&self) -> Option<ActionKind> {
        match (&self.source, &self.installed) {
            (
                StoreSource::Tilo { .. },
                InstalledState::Installed {
                    user_managed: true, ..
                },
            ) => Some(ActionKind::Remove),
            (
                StoreSource::Flathub | StoreSource::FlatpakRemote { .. },
                InstalledState::Installed {
                    user_managed: true, ..
                },
            ) => Some(ActionKind::FlatpakRemove),
            _ => None,
        }
    }

    /// Flatpak device repositories and device installs are read-only here.
    pub fn managed_by_device(&self, action: ActionKind) -> bool {
        match action {
            ActionKind::FlatpakInstall => matches!(
                self.source,
                StoreSource::FlatpakRemote {
                    user_managed: false,
                    ..
                }
            ),
            ActionKind::FlatpakUpdate | ActionKind::FlatpakRemove => matches!(
                self.installed,
                InstalledState::Installed {
                    user_managed: false,
                    ..
                }
            ),
            _ => false,
        }
    }

    /// The same availability contract for controls, rows and direct invocation.
    pub fn unavailable_action(&self, action: ActionKind) -> Option<(&'static str, String)> {
        if action == ActionKind::RuntimeInstall && self.installed.is_installed() {
            Some((
                "runtime_already_installed",
                ui::i18n!("store-runtime-already-installed"),
            ))
        } else if self.managed_by_device(action) {
            Some((
                "device_managed_repository",
                ui::i18n!("store-remote-managed"),
            ))
        } else if self.awaiting_developer
            && matches!(
                action,
                ActionKind::Open | ActionKind::Update | ActionKind::RuntimeInstall
            )
        {
            Some((
                "waiting_for_developer",
                ui::i18n!("store-waiting-for-developer"),
            ))
        } else if matches!(
            action,
            ActionKind::Open | ActionKind::Update | ActionKind::RuntimeInstall
        ) && matches!(self.source, StoreSource::Tilo { .. })
            && self.versions.is_empty()
            && !self.installed.is_installed()
        {
            Some((
                "device_version_unavailable",
                ui::i18n!("store-device-version-unavailable"),
            ))
        } else {
            None
        }
    }

    /// Whether an action is still valid for this exact catalogue/install
    /// snapshot. Resource operations re-check this at execution time so a
    /// stale agent result cannot turn an old "Remove" or "Update" offer into
    /// a different package-management operation.
    pub fn allows_action(&self, action: ActionKind, flatpak_available: bool) -> bool {
        self.unavailable_action(action).is_none()
            && (!action.requires_flatpak_host() || flatpak_available)
            && (self.action_kind(flatpak_available) == action
                || self.pin_action() == Some(action)
                || self.remove_action() == Some(action))
    }

    /// Whether a card should offer a secondary **Remove**.
    pub fn offers_remove(&self) -> bool {
        self.remove_action().is_some()
    }

    /// Ranking rank: pinned first, then merely-cached, then the
    /// rest. Higher sorts earlier. Drives the "mark installed →
    /// rank first" behaviour.
    pub fn install_rank(&self) -> u8 {
        if self.installed.is_pinned() {
            2
        } else {
            u8::from(self.installed.is_installed())
        }
    }
}

/// Each item's relevance to `needle`, case- and accent-insensitively —
/// higher is better, `None` when neither its name nor its description
/// matches. A hit in the name outranks a hit in the description; a prefix
/// outranks a mid-string hit. An empty needle matches everything (so the
/// search bar can start empty). `index` keeps the items indexed while they
/// stay the same.
pub fn relevance(index: &ListIndex, needle: &str, items: &[&StoreItem]) -> Vec<Option<i32>> {
    index.ranks(needle, items.iter().map(|item| searchable_fields(item)))
}

/// Catalogue metadata searched by both the app's list/worker and the system
/// directory. Names keep their title rank; identities and categories remain
/// discoverable without outranking the app a person named.
pub(crate) fn searchable_fields(item: &StoreItem) -> impl Iterator<Item = (&str, FieldWeight)> {
    let source_name = match &item.source {
        StoreSource::Tilo { registry_name } => registry_name.as_str(),
        StoreSource::Flathub => "Flathub",
        StoreSource::FlatpakRemote { name, .. } => name.as_str(),
    };
    [
        (item.name.as_str(), FieldWeight::TITLE),
        (item.id.as_str(), FieldWeight::IDENTITY),
        (item.description.as_str(), FieldWeight::BODY),
        (item.tagline.as_str(), FieldWeight::SUBTITLE),
        (
            item.publisher_pubkey_hex.as_deref().unwrap_or(""),
            FieldWeight::IDENTITY,
        ),
        (source_name, FieldWeight::METADATA),
        (item.source.distribution_name(), FieldWeight::METADATA),
    ]
    .into_iter()
    .chain(
        item.categories
            .iter()
            .map(|category| (category.as_str(), FieldWeight::METADATA)),
    )
}

/// Which source-side filter chip is active.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SourceFilter {
    /// Show everything.
    All,
    /// Only Tilo registry entries.
    Tilo,
    /// Only Flatpak entries.
    Flatpak,
    /// Items already present on this device: cached Tilo apps or installed
    /// Flatpaks. The variant name remains serialized for compatibility.
    Installed,
    /// Installed apps a source offers a newer version of, the system
    /// image's own apps included.
    Updates,
}

impl SourceFilter {
    pub const ALL: &'static [SourceFilter] = &[
        SourceFilter::All,
        SourceFilter::Tilo,
        SourceFilter::Flatpak,
        SourceFilter::Installed,
        SourceFilter::Updates,
    ];

    pub fn label(self) -> &'static str {
        match self {
            SourceFilter::All => "All",
            SourceFilter::Tilo => "Tilo",
            SourceFilter::Flatpak => "Flatpak",
            SourceFilter::Installed => "On device",
            SourceFilter::Updates => "Updates",
        }
    }

    /// Does an item pass this filter?
    pub fn accepts(self, item: &StoreItem) -> bool {
        match self {
            SourceFilter::All => true,
            SourceFilter::Tilo => matches!(item.source, StoreSource::Tilo { .. }),
            SourceFilter::Flatpak => matches!(
                item.source,
                StoreSource::Flathub | StoreSource::FlatpakRemote { .. }
            ),
            SourceFilter::Installed => !matches!(item.installed, InstalledState::Not),
            SourceFilter::Updates => item.installed.update_available(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tilo_item(name: &str, installed: InstalledState, latest: &str) -> StoreItem {
        StoreItem {
            bot: None,
            awaiting_developer: false,
            listing: crate::model::Listing::default(),
            id: format!("so.tilo.{}", name.to_ascii_lowercase()),
            name: name.into(),
            description: format!("Some {name} app"),
            tagline: String::new(),
            supported_languages: Vec::new(),
            categories: vec!["Productivity".into()],
            icon_url: None,
            screenshot_urls: Vec::new(),
            homepage_url: None,
            source_url: None,
            install_ref_path: None,
            source: StoreSource::Tilo {
                registry_name: "Tilo main".into(),
            },
            trust: StoreTrust::PublisherSigned,
            versions: vec![StoreItemVersion {
                version: latest.into(),
                released_at_unix: 0,
                download_url: Some("https://x/y.tapp".into()),
                root_hash_hex: Some("de".repeat(32)),
                size_bytes: 100,
                runtime: None,
            }],
            publisher_pubkey_hex: Some("aa".repeat(32)),
            installed,
        }
    }

    fn flatpak_item(name: &str, installed: InstalledState) -> StoreItem {
        StoreItem {
            bot: None,
            awaiting_developer: false,
            listing: crate::model::Listing::default(),
            id: format!("org.example.{name}"),
            name: name.into(),
            description: format!("{name} on Flathub"),
            tagline: String::new(),
            supported_languages: Vec::new(),
            categories: vec!["Games".into()],
            icon_url: None,
            screenshot_urls: Vec::new(),
            homepage_url: None,
            source_url: None,
            install_ref_path: None,
            source: StoreSource::Flathub,
            trust: StoreTrust::Flathub,
            versions: vec![StoreItemVersion {
                version: "1.0".into(),
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

    fn qinstalled(version: &str, available: &str, pinned: bool) -> InstalledState {
        InstalledState::Installed {
            version: version.into(),
            available: available.into(),
            pinned,
            user_managed: true,
        }
    }

    /// Action kind, progress labels, per-source removal, authorisation and update detection.
    #[test]
    fn item_actions_follow_source_and_install_state() {
        // Case `action_kind_reflects_source_and_install_state`.
        {
            // Tilo not-installed → Open (fetch-on-demand + run), with Keep
            // on this device beside it as the opt-in.
            let not_installed = tilo_item("Notes", InstalledState::Not, "0.1.0");
            assert_eq!(not_installed.action_kind(true), ActionKind::Open);
            assert_eq!(not_installed.pin_action(), Some(ActionKind::MarkInstalled));

            // Tilo installed + up-to-date → still Open (relaunch), and it
            // now offers the pin toggle + a secondary Remove.
            let up_to_date = tilo_item("Notes", qinstalled("0.1.0", "0.1.0", false), "0.1.0");
            assert_eq!(up_to_date.action_kind(true), ActionKind::Open);
            assert_eq!(up_to_date.pin_action(), Some(ActionKind::MarkInstalled));
            assert!(up_to_date.offers_remove());

            // Tilo installed but newer available → Update.
            let stale = tilo_item("Notes", qinstalled("0.1.0", "0.2.0", false), "0.2.0");
            assert_eq!(stale.action_kind(true), ActionKind::Update);

            // Tilo pinned → pin toggle flips to Unmark.
            let pinned = tilo_item("Notes", qinstalled("0.1.0", "0.1.0", true), "0.1.0");
            assert_eq!(pinned.pin_action(), Some(ActionKind::Unmark));

            let flatpak_not_installed = flatpak_item("Chess", InstalledState::Not);
            assert_eq!(
                flatpak_not_installed.action_kind(true),
                ActionKind::FlatpakInstall
            );
            assert_eq!(
                flatpak_not_installed.action_kind(false),
                ActionKind::NeedsFlatpakHost
            );
            assert!(flatpak_not_installed.pin_action().is_none());

            let flatpak_installed = flatpak_item("Chess", qinstalled("1.0", "1.0", false));
            assert_eq!(
                flatpak_installed.action_kind(true),
                ActionKind::FlatpakOpen,
                "an installed app's primary action is to open it — a store \
             that can install a Flatpak and then not start it is not \
             finished"
            );

            let update = flatpak_item("Chess", qinstalled("0.9", "1.0", false));
            assert_eq!(update.action_kind(true), ActionKind::FlatpakUpdate);
            assert_eq!(
                flatpak_installed.remove_action(),
                Some(ActionKind::FlatpakRemove),
                "removal moves to the secondary slot, and is Flatpak's own \
             removal rather than tilo-install's"
            );
        }

        // Case `progress_labels_name_the_offered_operation`.
        {
            assert_eq!(ActionKind::Open.progress_label(), "Starting");
            assert_eq!(ActionKind::FlatpakInstall.progress_label(), "Installing");
            assert_eq!(ActionKind::FlatpakRemove.progress_label(), "Removing");
        }

        // Case `each_source_names_its_own_removal`.
        // The two sources remove differently, and the item is what knows
        // which. Routing a Flatpak through `tilo-install uninstall` would
        // report "no such package installed" and leave it there.
        {
            let q = tilo_item("Notes", qinstalled("1", "1", false), "1");
            assert_eq!(q.remove_action(), Some(ActionKind::Remove));
            let f = flatpak_item("Chess", qinstalled("1", "1", false));
            assert_eq!(f.remove_action(), Some(ActionKind::FlatpakRemove));
            // Nothing to remove until something is installed.
            assert_eq!(
                tilo_item("Notes", InstalledState::Not, "1").remove_action(),
                None
            );
            assert_eq!(
                flatpak_item("Chess", InstalledState::Not).remove_action(),
                None
            );
        }

        // Case `action_authorization_tracks_the_current_install_snapshot`.
        {
            let not_installed = tilo_item("Notes", InstalledState::Not, "1");
            assert!(not_installed.allows_action(ActionKind::Open, true));
            assert!(!not_installed.allows_action(ActionKind::Remove, true));
            assert!(
                not_installed.allows_action(ActionKind::MarkInstalled, true),
                "keeping an app not downloaded yet downloads it to keep"
            );

            let cached = tilo_item("Notes", qinstalled("1", "1", false), "1");
            assert!(cached.allows_action(ActionKind::Open, true));
            assert!(cached.allows_action(ActionKind::Remove, true));
            assert!(cached.allows_action(ActionKind::MarkInstalled, true));
            assert!(!cached.allows_action(ActionKind::Update, true));
        }

        // Case `update_available_only_for_a_newer_version`.
        {
            let same = qinstalled("1.0.0", "1.0.0", false);
            assert!(!same.update_available());
            let newer = qinstalled("0.1.0", "0.1.62530+gabc", false);
            assert!(newer.update_available());
            let older = qinstalled("0.1.62530+gabc", "0.1.0", false);
            assert!(
                !older.update_available(),
                "a catalogue behind the system image offers no downgrade"
            );
            assert!(!qinstalled("1.0.0", "", false).update_available());
            assert!(!InstalledState::Not.update_available());
            let stale = tilo_item("Notes", qinstalled("0.1.0", "0.2.0", false), "0.2.0");
            let current = tilo_item("Clocks", qinstalled("0.2.0", "0.2.0", false), "0.2.0");
            assert!(SourceFilter::Updates.accepts(&stale));
            assert!(!SourceFilter::Updates.accepts(&current));
            assert_eq!(SourceFilter::Updates.label(), "Updates");
        }
    }

    /// Install rank, search matching, relevance scoring and source filters.
    #[test]
    fn catalogue_ranking_search_and_source_filters() {
        // Case `install_rank_orders_pinned_first`.
        {
            let not = tilo_item("A", InstalledState::Not, "1");
            let cached = tilo_item("B", qinstalled("1", "1", false), "1");
            let pinned = tilo_item("C", qinstalled("1", "1", true), "1");
            assert!(pinned.install_rank() > cached.install_rank());
            assert!(cached.install_rank() > not.install_rank());
        }

        // Case `search_matches_name_and_description_case_insensitive`.
        {
            let n = tilo_item("Notes", InstalledState::Not, "0.1.0");
            let index = ListIndex::default();
            let matches = |needle: &str| relevance(&index, needle, &[&n])[0].is_some();
            assert!(matches(""));
            assert!(matches("Notes"));
            assert!(matches("notes"));
            assert!(matches("some notes app"));
            assert!(!matches("terminal"));
        }

        // Case `relevance_prefers_name_prefix_hits`.
        {
            let mut n = tilo_item("Notes", InstalledState::Not, "0.1.0");
            n.description = "great little app".into();
            let index = ListIndex::default();
            let rank = |needle: &str| relevance(&index, needle, &[&n])[0];
            let prefix = rank("no");
            // A match inside a word takes four letters (shared search).
            let mid = rank("otes");
            let desc = rank("little");
            assert!(mid.is_some() && desc.is_some());
            assert!(
                prefix > mid,
                "prefix ({prefix:?}) should outrank mid-name ({mid:?})"
            );
            assert!(
                prefix > desc,
                "prefix ({prefix:?}) should outrank description ({desc:?})"
            );
        }

        // Case `source_filter_gates_items`.
        {
            let q = tilo_item("A", InstalledState::Not, "1");
            let f = flatpak_item("B", InstalledState::Not);
            let installed_q = tilo_item("C", qinstalled("1", "1", false), "1");
            assert!(SourceFilter::All.accepts(&q));
            assert!(SourceFilter::All.accepts(&f));
            assert!(SourceFilter::Tilo.accepts(&q));
            assert!(!SourceFilter::Tilo.accepts(&f));
            assert!(SourceFilter::Flatpak.accepts(&f));
            assert!(!SourceFilter::Flatpak.accepts(&q));
            assert!(SourceFilter::Installed.accepts(&installed_q));
            assert!(!SourceFilter::Installed.accepts(&q));
        }
    }
}
