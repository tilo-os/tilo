//! Pure data model for the Software store.
//!
//! Nothing in this module touches the CRDT, the renderer, or the
//! filesystem — it is the in-memory description of the catalog plus the
//! pure logic the UI layer leans on (filtering, install-state derivation,
//! formatting). The *mutable* state (which apps are installed, which are
//! mid-install) lives in the Automerge document (`doc.rs`); this module
//! only describes the immutable catalog and derives a per-app
//! [`InstallState`] from a snapshot of that document.

use std::collections::BTreeMap;

/// Stable identifier for a catalog entry. We use the reverse-DNS app id
/// (the Flatpak / AppStream convention, e.g. `org.gimp.GIMP`) so the
/// sample catalog lines up with what a real `flatpak list` would report
/// and the live probe can merge by the same key.
pub type AppId = String;

/// Top-level browse categories. The order here is the order the chips
/// render in on the home screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Category {
    Productivity,
    Development,
    Graphics,
    Games,
    Internet,
}

impl Category {
    /// Every category, in display order. `All` is handled separately by
    /// the UI as a leading pseudo-chip.
    pub const ALL: [Category; 5] = [
        Category::Productivity,
        Category::Development,
        Category::Graphics,
        Category::Games,
        Category::Internet,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Category::Productivity => "Productivity",
            Category::Development => "Development",
            Category::Graphics => "Graphics",
            Category::Games => "Games",
            Category::Internet => "Internet",
        }
    }

    /// A short single glyph / pictograph used on the category chip.
    /// Picked from a conservative set that renders under the bundled
    /// sans / system fallback.
    pub fn glyph(self) -> &'static str {
        match self {
            Category::Productivity => "◆",
            Category::Development => "‹›",
            Category::Graphics => "✸",
            Category::Games => "⬤",
            Category::Internet => "◯",
        }
    }
}

/// Which packaging backend an app ships through. Drives the small
/// provenance line on the detail page and the (gated) install command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// Flatpak from a remote (Flathub in practice).
    Flatpak,
    /// An OCI container image pulled with podman.
    Oci,
    /// A qOS-native app — distributed as a cached, strongly-sandboxed
    /// bundle rather than a permanent install. Drives the bespoke
    /// "Open directly" / "Add" affordances (see [`QosAppModel`]).
    QosNative,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Source::Flatpak => "Flatpak · Flathub",
            Source::Oci => "OCI · podman",
            Source::QosNative => "qOS app · cached bundle",
        }
    }
}

/// The two bespoke affordances qOS offers for its own native apps, in place
/// of the permanent Flatpak install. **Rendered fully in the UI; the
/// backend behaviour is a documented stub for now** (see the worker's
/// `QosOpenDirect` / `QosAdd` request handling).
///
/// * [`QosAppModel::OpenDirect`] — run the app *now* from a cached,
///   web-app-style bundle inside a strong per-launch sandbox, without a
///   permanent install. Think "open this once" rather than "keep it".
/// * [`QosAppModel::Add`] — keep the cached bundle and *prioritise* it in
///   background auto-updates and global search, the closest thing to a
///   permanent install in the qOS model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QosAppModel {
    OpenDirect,
    Add,
}

impl QosAppModel {
    pub fn label(self) -> &'static str {
        match self {
            QosAppModel::OpenDirect => "Open directly",
            QosAppModel::Add => "Add",
        }
    }

    /// The explanatory tooltip shown on hover, surfacing the qOS semantics
    /// so the affordance isn't mistaken for a normal install.
    pub fn tooltip(self) -> &'static str {
        match self {
            QosAppModel::OpenDirect => {
                "Run this app right now from a cached, sandboxed bundle — \
                 like a web app. Nothing is permanently installed."
            }
            QosAppModel::Add => {
                "Keep this app cached and prioritise it in auto-updates and \
                 global search. The qOS equivalent of installing."
            }
        }
    }
}

/// One screenshot for the detail strip.
///
/// When `url` is set the store fetches + decodes the real bitmap off-thread
/// (via the shared [`ui::ImageStore`]) and paints it, showing an honest
/// loading skeleton until it resolves. The `tone_a` / `tone_b` palette
/// indices are the fallback: a duotone faux-UI thumbnail drawn when no real
/// image is available (or a fetch fails), so the strip is never a blank grey
/// bar.
#[derive(Clone, Debug)]
pub struct Shot {
    /// Index into the theme accent ramp for the dominant fill (fallback).
    pub tone_a: usize,
    /// Index into the theme neutral ramp for the secondary fill (fallback).
    pub tone_b: usize,
    pub caption: String,
    /// URL (or `qos-demo://…` synthesised source) of the real screenshot
    /// bitmap, when known. `None` → paint the duotone fallback.
    pub url: Option<String>,
}

/// A short textual review with a star rating, shown on the detail page.
/// Flathub exposes no reviews API, so these are only carried by the
/// bundled sample catalog (live entries show download counts instead).
#[derive(Clone, Debug, PartialEq)]
pub struct Review {
    pub author: String,
    pub stars: f32,
    pub body: String,
}

impl Review {
    pub fn new(author: &str, stars: f32, body: &str) -> Self {
        Self { author: author.into(), stars, body: body.into() }
    }
}

/// A compatibility notice surfaced on cards / detail pages: a short,
/// human-readable caveat about running the app on qOS (e.g. "Requests host
/// command execution — runs outside the cluster sandbox").
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Compat {
    pub label: String,
    /// `true` → a hard incompatibility / strong warning (red); `false` → an
    /// informational notice (neutral).
    pub blocking: bool,
}

impl Compat {
    pub fn info(label: &str) -> Self {
        Self { label: label.into(), blocking: false }
    }
    pub fn warn(label: &str) -> Self {
        Self { label: label.into(), blocking: true }
    }
}

/// One immutable catalog entry.
#[derive(Clone, Debug)]
pub struct AppEntry {
    pub id: AppId,
    pub name: String,
    pub developer: String,
    /// One-line summary used on cards.
    pub summary: String,
    /// Full multi-paragraph description used on the detail page.
    pub description: String,
    pub category: Category,
    pub source: Source,
    /// The newest version available in the catalog. The *installed*
    /// version (held in the CRDT) is compared against this to decide
    /// whether an update is pending.
    pub latest_version: String,
    /// Download size in mebibytes (used for the size pill).
    pub size_mib: f64,
    /// Average user rating, 0.0..=5.0.
    pub rating: f32,
    /// Number of ratings the average is computed from.
    pub rating_count: u32,
    /// Sandbox permissions the app requests (human-readable).
    pub permissions: Vec<Permission>,
    /// Screenshot strip for the detail page.
    pub shots: Vec<Shot>,
    /// Whether the curators flagged this as a featured / editor's-choice
    /// app (drives the large hero cards on the home screen).
    pub featured: bool,
    /// Palette index (into the accent ramp) for this app's icon tile, so
    /// even icon colour tracks the active theme. Used as the fallback tile
    /// colour behind the monogram when no real icon is available.
    pub icon_tone: usize,
    /// One- or two-character monogram drawn on the icon tile when no real
    /// icon bitmap is available (or its fetch failed).
    pub monogram: String,
    /// URL (or `qos-demo://…` synthesised source) of the real app icon, when
    /// known (from the Flathub/AppStream metadata). The store fetches +
    /// decodes it off-thread and paints it; `None` (or a failed fetch) falls
    /// back to the monogram tile.
    pub icon_url: Option<String>,
    /// Textual reviews (sample catalog only — see [`Review`]).
    pub reviews: Vec<Review>,
    /// Compatibility notices for this app on qOS.
    pub compat: Vec<Compat>,
    /// Lifetime / monthly download count from Flathub stats, when known
    /// (live entries). Surfaced as social proof alongside the rating.
    pub download_count: Option<u64>,
    /// The remote this entry installs from (e.g. `flathub`). Drives the
    /// gated install command; defaults to `flathub`.
    pub remote: String,
}

impl AppEntry {
    /// A minimal Flathub-sourced entry built from a search hit / AppStream
    /// record, with neutral defaults the caller fills in. Used by the
    /// async worker (`flatpak.rs`) to construct live catalog entries.
    pub fn flathub_stub(
        id: &str,
        name: &str,
        developer: &str,
        summary: &str,
        category: Category,
    ) -> Self {
        // Derive a stable icon tone from the id so the tile colour is
        // deterministic per app without a real icon bitmap.
        let tone = (id.bytes().map(|b| b as usize).sum::<usize>() % 9) + 2;
        Self {
            id: id.into(),
            name: name.into(),
            developer: developer.into(),
            summary: summary.into(),
            description: summary.into(),
            category,
            source: Source::Flatpak,
            latest_version: String::new(),
            size_mib: 0.0,
            rating: 4.0,
            rating_count: 0,
            permissions: Vec::new(),
            shots: Vec::new(),
            featured: false,
            icon_tone: tone,
            monogram: name.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_default(),
            icon_url: None,
            reviews: Vec::new(),
            compat: Vec::new(),
            download_count: None,
            remote: "flathub".into(),
        }
    }

    /// `true` for a qOS-native app, which uses the bespoke
    /// "Open directly" / "Add" affordances instead of a permanent install.
    pub fn is_qos_native(&self) -> bool {
        self.source == Source::QosNative
    }

    /// `1.2M downloads` / `840k downloads` style count, when known.
    pub fn download_label(&self) -> Option<String> {
        let n = self.download_count?;
        Some(if n >= 1_000_000 {
            format!("{:.1}M downloads", n as f64 / 1_000_000.0)
        } else if n >= 1_000 {
            format!("{:.0}k downloads", n as f64 / 1_000.0)
        } else {
            format!("{n} downloads")
        })
    }

    /// A compact `1.2 GiB` / `412 MiB` size string.
    pub fn size_label(&self) -> String {
        if self.size_mib >= 1024.0 {
            format!("{:.1} GiB", self.size_mib / 1024.0)
        } else {
            format!("{:.0} MiB", self.size_mib)
        }
    }

    /// `4.6` style one-decimal rating.
    pub fn rating_label(&self) -> String {
        format!("{:.1}", self.rating)
    }

    /// `12.4k ratings` / `840 ratings` style count.
    pub fn rating_count_label(&self) -> String {
        let n = self.rating_count;
        if n >= 1000 {
            format!("{:.1}k ratings", n as f64 / 1000.0)
        } else {
            format!("{} ratings", n)
        }
    }

    /// True if `query` (already lower-cased) matches the name, developer
    /// or summary. Empty query matches everything.
    pub fn matches(&self, query: &str) -> bool {
        if query.is_empty() {
            return true;
        }
        self.name.to_lowercase().contains(query)
            || self.developer.to_lowercase().contains(query)
            || self.summary.to_lowercase().contains(query)
            || self.id.to_lowercase().contains(query)
    }
}

/// A requested sandbox capability. Mirrors the kind of line a Flatpak
/// permission summary shows; the renderer pairs each with a small dot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Permission {
    pub label: String,
    /// Whether this permission is considered sensitive (network, device,
    /// full filesystem) — drives a warning tint instead of the neutral
    /// tint.
    pub sensitive: bool,
}

impl Permission {
    pub fn normal(label: &str) -> Self {
        Self { label: label.into(), sensitive: false }
    }
    pub fn sensitive(label: &str) -> Self {
        Self { label: label.into(), sensitive: true }
    }
}

/// A snapshot of the mutable install state, read out of the CRDT once
/// per frame. `installed` maps app-id → the *installed* version string;
/// `installing` maps app-id → progress in 0.0..=1.0.
#[derive(Clone, Debug, Default)]
pub struct InstallSnapshot {
    pub installed: BTreeMap<AppId, String>,
    pub installing: BTreeMap<AppId, f64>,
}

/// The derived per-app state the UI renders against.
#[derive(Clone, Debug, PartialEq)]
pub enum InstallState {
    /// Not installed and not in flight.
    Available,
    /// Currently downloading / installing; carries 0.0..=1.0 progress.
    Installing(f64),
    /// Installed and at the latest version.
    Installed,
    /// Installed but an update is available (carries the installed
    /// version that is behind `latest_version`).
    UpdateAvailable(String),
}

impl InstallState {
    pub fn is_installed_or_update(&self) -> bool {
        matches!(self, InstallState::Installed | InstallState::UpdateAvailable(_))
    }
}

/// The phase a single in-flight install / update is in, derived purely
/// from its 0.0..=1.0 progress. A real Flatpak transaction *downloads* the
/// objects first, then *installs* (deploys) them; we split the bar at 0.6
/// so the Updates view can show which step each app is on without needing
/// a second progress channel from the worker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstallPhase {
    /// Queued, not yet started (progress ~0, not the active item).
    Queued,
    /// Fetching the objects from the remote.
    Downloading,
    /// Deploying / committing the fetched objects.
    Installing,
}

impl InstallPhase {
    /// The boundary between the download and install phases, as a fraction
    /// of total progress. Below this the app is downloading; at or above it
    /// the app is installing.
    pub const DEPLOY_AT: f64 = 0.6;

    /// Derive the phase from a 0.0..=1.0 progress value. `queued` marks an
    /// app that is enqueued but not yet the active (progressing) item, so a
    /// 0%-progress queued app reads as "Queued" rather than "Downloading".
    pub fn from_progress(p: f64, queued: bool) -> Self {
        if queued {
            InstallPhase::Queued
        } else if p < Self::DEPLOY_AT {
            InstallPhase::Downloading
        } else {
            InstallPhase::Installing
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            InstallPhase::Queued => "Queued",
            InstallPhase::Downloading => "Downloading",
            InstallPhase::Installing => "Installing",
        }
    }
}

/// A summary of an in-progress update / install batch, derived for the
/// Updates view's overall status line. Pure: built from the per-app
/// counts the UI already has, so it is unit-testable without a renderer.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct UpdateProgress {
    /// Apps enqueued but not yet started.
    pub queued: usize,
    /// Apps actively downloading or installing right now.
    pub active: usize,
    /// Apps finished in this batch.
    pub done: usize,
    /// Mean progress across queued+active+done, 0.0..=1.0 (done counts as
    /// 1.0, queued as 0.0). Used for the overall bar.
    pub overall: f64,
}

impl UpdateProgress {
    /// Total apps participating in the batch.
    pub fn total(&self) -> usize {
        self.queued + self.active + self.done
    }

    /// `true` while any app is queued or active — i.e. the batch is still
    /// running and the overall status line / bar should show.
    pub fn in_flight(&self) -> bool {
        self.queued + self.active > 0
    }

    /// A short human-readable overall status, e.g.
    /// `Updating 2 of 5 · 1 queued` or `All updates complete`.
    pub fn status_line(&self) -> String {
        if self.total() == 0 {
            return String::new();
        }
        if !self.in_flight() {
            return if self.done == 1 {
                "Update complete".into()
            } else {
                format!("{} updates complete", self.done)
            };
        }
        let finished = self.done;
        let total = self.total();
        let mut s = format!("Updating {} of {}", (finished + self.active).min(total), total);
        if self.queued > 0 {
            s.push_str(&format!(" · {} queued", self.queued));
        }
        s
    }
}

/// The immutable catalog plus the pure queries the UI runs against it.
#[derive(Clone, Debug, Default)]
pub struct Catalog {
    pub apps: Vec<AppEntry>,
}

impl Catalog {
    pub fn new(apps: Vec<AppEntry>) -> Self {
        Self { apps }
    }

    pub fn get(&self, id: &str) -> Option<&AppEntry> {
        self.apps.iter().find(|a| a.id == id)
    }

    pub fn len(&self) -> usize {
        self.apps.len()
    }

    pub fn is_empty(&self) -> bool {
        self.apps.is_empty()
    }

    /// Derive the install state for `app` from a snapshot.
    pub fn state_of(&self, app: &AppEntry, snap: &InstallSnapshot) -> InstallState {
        if let Some(p) = snap.installing.get(&app.id) {
            return InstallState::Installing((*p).clamp(0.0, 1.0));
        }
        match snap.installed.get(&app.id) {
            Some(v) if *v == app.latest_version => InstallState::Installed,
            Some(v) => InstallState::UpdateAvailable(v.clone()),
            None => InstallState::Available,
        }
    }

    /// All featured apps, in catalog order.
    pub fn featured(&self) -> Vec<&AppEntry> {
        self.apps.iter().filter(|a| a.featured).collect()
    }

    /// Apps in `category`, in catalog order.
    pub fn in_category(&self, cat: Category) -> Vec<&AppEntry> {
        self.apps.iter().filter(|a| a.category == cat).collect()
    }

    /// Apps matching the lower-cased `query`, optionally constrained to a
    /// single category.
    pub fn search<'a>(&'a self, query: &str, cat: Option<Category>) -> Vec<&'a AppEntry> {
        let q = query.to_lowercase();
        self.apps
            .iter()
            .filter(|a| cat.map_or(true, |c| a.category == c))
            .filter(|a| a.matches(&q))
            .collect()
    }

    /// Installed apps (including those with a pending update), in catalog
    /// order.
    pub fn installed<'a>(&'a self, snap: &InstallSnapshot) -> Vec<&'a AppEntry> {
        self.apps
            .iter()
            .filter(|a| self.state_of(a, snap).is_installed_or_update())
            .collect()
    }

    /// Apps that have an update pending.
    pub fn updatable<'a>(&'a self, snap: &InstallSnapshot) -> Vec<&'a AppEntry> {
        self.apps
            .iter()
            .filter(|a| matches!(self.state_of(a, snap), InstallState::UpdateAvailable(_)))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, version: &str) -> AppEntry {
        AppEntry {
            id: id.into(),
            name: id.into(),
            developer: "dev".into(),
            summary: "s".into(),
            description: "d".into(),
            category: Category::Productivity,
            source: Source::Flatpak,
            latest_version: version.into(),
            size_mib: 100.0,
            rating: 4.0,
            rating_count: 10,
            permissions: vec![],
            shots: vec![],
            featured: false,
            icon_tone: 5,
            monogram: "X".into(),
            icon_url: None,
            reviews: vec![],
            compat: vec![],
            download_count: None,
            remote: "flathub".into(),
        }
    }

    #[test]
    fn state_derivation_covers_all_cases() {
        let cat = Catalog::new(vec![entry("a", "2.0"), entry("b", "2.0"), entry("c", "2.0")]);
        let mut snap = InstallSnapshot::default();
        // a: installed at latest
        snap.installed.insert("a".into(), "2.0".into());
        // b: installed but behind
        snap.installed.insert("b".into(), "1.0".into());
        // c: installing
        snap.installing.insert("c".into(), 0.5);

        assert_eq!(cat.state_of(cat.get("a").unwrap(), &snap), InstallState::Installed);
        assert_eq!(
            cat.state_of(cat.get("b").unwrap(), &snap),
            InstallState::UpdateAvailable("1.0".into())
        );
        assert_eq!(cat.state_of(cat.get("c").unwrap(), &snap), InstallState::Installing(0.5));
    }

    #[test]
    fn installing_takes_priority_over_installed() {
        // If an app is both installed (old) and mid-update, the
        // installing state wins so the progress bar shows.
        let cat = Catalog::new(vec![entry("a", "2.0")]);
        let mut snap = InstallSnapshot::default();
        snap.installed.insert("a".into(), "1.0".into());
        snap.installing.insert("a".into(), 0.3);
        assert_eq!(cat.state_of(cat.get("a").unwrap(), &snap), InstallState::Installing(0.3));
    }

    #[test]
    fn updatable_and_installed_queries() {
        let cat = Catalog::new(vec![entry("a", "2.0"), entry("b", "2.0"), entry("c", "2.0")]);
        let mut snap = InstallSnapshot::default();
        snap.installed.insert("a".into(), "2.0".into());
        snap.installed.insert("b".into(), "1.0".into());
        assert_eq!(cat.installed(&snap).len(), 2);
        assert_eq!(cat.updatable(&snap).len(), 1);
        assert_eq!(cat.updatable(&snap)[0].id, "b");
    }

    #[test]
    fn search_filters_by_query_and_category() {
        let mut a = entry("org.app.Photo", "1.0");
        a.name = "Photo".into();
        a.category = Category::Graphics;
        let mut b = entry("org.app.Sheet", "1.0");
        b.name = "Sheet".into();
        b.category = Category::Productivity;
        let cat = Catalog::new(vec![a, b]);
        assert_eq!(cat.search("photo", None).len(), 1);
        assert_eq!(cat.search("", Some(Category::Productivity)).len(), 1);
        assert_eq!(cat.search("photo", Some(Category::Productivity)).len(), 0);
    }

    #[test]
    fn size_label_switches_units() {
        let mut a = entry("a", "1.0");
        a.size_mib = 512.0;
        assert_eq!(a.size_label(), "512 MiB");
        a.size_mib = 2048.0;
        assert_eq!(a.size_label(), "2.0 GiB");
    }

    #[test]
    fn install_phase_splits_at_deploy_boundary() {
        // A queued app reads as Queued regardless of (zero) progress.
        assert_eq!(InstallPhase::from_progress(0.0, true), InstallPhase::Queued);
        // Below the boundary → downloading; at/above → installing.
        assert_eq!(InstallPhase::from_progress(0.0, false), InstallPhase::Downloading);
        assert_eq!(InstallPhase::from_progress(0.59, false), InstallPhase::Downloading);
        assert_eq!(InstallPhase::from_progress(0.6, false), InstallPhase::Installing);
        assert_eq!(InstallPhase::from_progress(0.95, false), InstallPhase::Installing);
    }

    #[test]
    fn update_progress_status_line_reflects_batch() {
        // Mid-batch: 1 done, 1 active, 2 queued of 4.
        let p = UpdateProgress { queued: 2, active: 1, done: 1, overall: 0.4 };
        assert!(p.in_flight());
        assert_eq!(p.total(), 4);
        assert_eq!(p.status_line(), "Updating 2 of 4 · 2 queued");

        // Active only, nothing queued.
        let p = UpdateProgress { queued: 0, active: 2, done: 0, overall: 0.3 };
        assert_eq!(p.status_line(), "Updating 2 of 2");

        // Finished batch.
        let p = UpdateProgress { queued: 0, active: 0, done: 3, overall: 1.0 };
        assert!(!p.in_flight());
        assert_eq!(p.status_line(), "3 updates complete");

        // Empty batch → empty line.
        assert_eq!(UpdateProgress::default().status_line(), "");
    }
}
