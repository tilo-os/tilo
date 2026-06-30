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
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Source::Flatpak => "Flatpak · Flathub",
            Source::Oci => "OCI · podman",
        }
    }
}

/// One synthetic "screenshot" for the detail strip. We have no real
/// image assets, so a screenshot is described as a duotone placeholder
/// with a caption; the renderer paints a faux-UI thumbnail from the two
/// palette indices.
#[derive(Clone, Debug)]
pub struct Shot {
    /// Index into the theme accent ramp for the dominant fill.
    pub tone_a: usize,
    /// Index into the theme neutral ramp for the secondary fill.
    pub tone_b: usize,
    pub caption: String,
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
    /// even icon colour tracks the active theme.
    pub icon_tone: usize,
    /// One- or two-character monogram drawn on the icon tile.
    pub monogram: String,
}

impl AppEntry {
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
}
