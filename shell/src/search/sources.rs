//! Concrete [`SearchSource`] implementations.
//!
//! Every source materialises its item set into a `Vec<SearchItem>`
//! held on the source struct. A refresh is a full rebuild — cheap
//! for the on-disk and PATH sources, and rate-limited via
//! [`SearchSource::last_refreshed_at`] + a caller-side TTL for the
//! network sources.

use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// One source-agnostic result row.
///
/// The palette renders one card per item; the shell's action layer
/// runs [`SearchItem::action`] when the user presses Enter.
#[derive(Debug, Clone)]
pub struct SearchItem {
    pub id: String,
    pub name: String,
    pub description: String,
    pub icon: Option<PathBuf>,
    pub source: SourceKind,
    /// Free-form category tags — [`SearchIndex`](super::SearchIndex)
    /// concatenates them into the ranking haystack.
    pub categories: Vec<String>,
    pub action: SearchAction,
}

/// Kind of the source that produced the item. Doubles as the
/// category-chip identifier the palette chrome toggles on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SourceKind {
    InstalledQosApp,
    UninstalledQosApp,
    InstalledFlatpak,
    UninstalledFlatpak,
    PathCommand,
}

impl SourceKind {
    /// Enumerable order used by [`CategoryFilter`]. Kept stable
    /// because the bitmask relies on the bit index matching each
    /// variant's discriminant.
    pub const ALL: [SourceKind; 5] = [
        SourceKind::InstalledQosApp,
        SourceKind::UninstalledQosApp,
        SourceKind::InstalledFlatpak,
        SourceKind::UninstalledFlatpak,
        SourceKind::PathCommand,
    ];

    fn bit(self) -> u8 {
        match self {
            SourceKind::InstalledQosApp => 1 << 0,
            SourceKind::UninstalledQosApp => 1 << 1,
            SourceKind::InstalledFlatpak => 1 << 2,
            SourceKind::UninstalledFlatpak => 1 << 3,
            SourceKind::PathCommand => 1 << 4,
        }
    }

    /// The category chip label the palette paints.
    pub fn chip_label(self) -> &'static str {
        match self {
            SourceKind::InstalledQosApp | SourceKind::UninstalledQosApp => "qOS",
            SourceKind::InstalledFlatpak | SourceKind::UninstalledFlatpak => "Flatpak",
            SourceKind::PathCommand => "Commands",
        }
    }
}

/// Bitmask over [`SourceKind`]. Palette chip clicks translate
/// straight into `with` / `without` calls on this mask.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CategoryFilter(u8);

impl CategoryFilter {
    pub fn empty() -> Self {
        Self(0)
    }

    pub fn all() -> Self {
        let mut mask = 0u8;
        for k in SourceKind::ALL {
            mask |= k.bit();
        }
        Self(mask)
    }

    pub fn with(mut self, kind: SourceKind) -> Self {
        self.0 |= kind.bit();
        self
    }

    pub fn without(mut self, kind: SourceKind) -> Self {
        self.0 &= !kind.bit();
        self
    }

    pub fn allows(self, kind: SourceKind) -> bool {
        (self.0 & kind.bit()) != 0
    }

    pub fn toggle(mut self, kind: SourceKind) -> Self {
        self.0 ^= kind.bit();
        self
    }
}

/// The action a result row runs on Enter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchAction {
    /// Launch an already-installed qOS app or Flatpak. `kind` picks
    /// the launcher backend (`qos-install run` vs `flatpak run`).
    LaunchInstalled {
        pkg_id: String,
        kind: SourceKind,
    },
    /// Download + install a qOS app from a registry. The palette
    /// rendering paints a "+" affordance in the row.
    AddQosApp {
        pkg_id: String,
        registry_id: String,
        qpkg_url: String,
        root_hash_hex: String,
    },
    /// Install a Flatpak from the user's remote (`flatpak install --user`).
    InstallFlatpak { flatpak_id: String },
    /// Run a `$PATH` command in the active workspace's terminal (or
    /// spawn a fresh one if the workspace has none open).
    RunCommand { cmd: PathBuf },
}

/// Kind-tagged error type returned by [`SearchSource::refresh`].
#[derive(Debug, Clone, thiserror::Error)]
pub enum SourceError {
    #[error("io: {0}")]
    Io(String),
    #[error("parse: {0}")]
    Parse(String),
    #[error("fetch: {0}")]
    Fetch(String),
    #[error("unavailable: {0}")]
    Unavailable(String),
}

impl From<io::Error> for SourceError {
    fn from(e: io::Error) -> Self {
        Self::Io(e.to_string())
    }
}

/// One search-source. Concrete impls cache items internally so a
/// palette rebuild is O(items) rather than O(fetch).
pub trait SearchSource: Send + Sync {
    fn kind(&self) -> SourceKind;
    fn items(&self) -> &[SearchItem];
    /// Rebuild the item cache. May be a no-op if the underlying
    /// data hasn't changed. Callers should treat this as blocking
    /// I/O and drive it off a background task when possible.
    fn refresh(&mut self) -> Result<(), SourceError>;
    /// When the last successful refresh completed. `None` on a
    /// never-refreshed source. Aggregators use this + a TTL to skip
    /// no-op fetches.
    fn last_refreshed_at(&self) -> Option<Instant>;
}

/// Recommended per-source refresh TTL. The palette caller uses this
/// as the default cutoff for "should I refetch on palette-open?".
pub const DEFAULT_REFRESH_TTL: Duration = Duration::from_secs(5 * 60);

pub mod installed_qos_apps;
pub mod installed_flatpak;
pub mod path_commands;
pub mod uninstalled_flatpak;
pub mod uninstalled_qos_apps;

#[doc(hidden)]
pub mod testing {
    //! Test helpers exposed so integration tests in other crates
    //! (currently just the shell's `#[cfg(test)]` module) can build
    //! deterministic sources without spinning a real filesystem or
    //! HTTP server.

    use super::*;

    pub struct MockSource {
        kind: SourceKind,
        items: Vec<SearchItem>,
        last_refresh: Option<Instant>,
        next_refresh_error: Option<SourceError>,
    }

    impl MockSource {
        pub fn new(kind: SourceKind) -> Self {
            Self {
                kind,
                items: Vec::new(),
                last_refresh: None,
                next_refresh_error: None,
            }
        }

        pub fn push(&mut self, item: SearchItem) {
            self.items.push(item);
        }

        pub fn fail_next_refresh(&mut self, e: SourceError) {
            self.next_refresh_error = Some(e);
        }
    }

    impl SearchSource for MockSource {
        fn kind(&self) -> SourceKind {
            self.kind
        }

        fn items(&self) -> &[SearchItem] {
            &self.items
        }

        fn refresh(&mut self) -> Result<(), SourceError> {
            if let Some(e) = self.next_refresh_error.take() {
                return Err(e);
            }
            self.last_refresh = Some(Instant::now());
            Ok(())
        }

        fn last_refreshed_at(&self) -> Option<Instant> {
            self.last_refresh
        }
    }
}

/// Common helper: expand `~` and read the file at `path` into a
/// UTF-8 string. Missing-file yields `Ok(None)` so callers can treat
/// a fresh user (no `registry.toml` yet) as "no items", not an
/// error.
pub(crate) fn read_optional_toml(path: &Path) -> Result<Option<String>, SourceError> {
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(SourceError::Io(format!("{}: {e}", path.display()))),
    }
}
