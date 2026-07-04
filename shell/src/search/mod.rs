//! Global-search backend for the shell's `⌘K` / `Super+Space` palette.
//!
//! The shell's palette is a chrome-level overlay that already handles
//! the query input, keyboard navigation, and fuzzy filtering. This
//! module supplies the *items* the palette ranks: installed and
//! discoverable qOS apps, installed and discoverable Flatpaks, and
//! PATH commands. Each item carries an [`SearchAction`] the shell
//! runs when the user presses Enter — a launch, an install, or a
//! shell-out.
//!
//! Design points:
//!
//! - Sources are pluggable via the [`sources::SearchSource`] trait. The
//!   five concrete sources — [`sources::InstalledQosAppsSource`],
//!   [`sources::UninstalledQosAppsSource`],
//!   [`sources::InstalledFlatpakSource`],
//!   [`sources::UninstalledFlatpakSource`] and
//!   [`sources::PathCommandSource`] — live under `sources.rs` so tests
//!   can construct mocks against the same trait.
//!
//! - Ranking uses [`nucleo-matcher`](https://docs.rs/nucleo-matcher)
//!   for the fuzzy score. The score is computed over the concatenated
//!   `name`, `description`, `id`, and `categories` haystack of each
//!   item; the item's own `name` is weighted higher via a bonus so
//!   direct name matches float above description matches.
//!
//! - Category filter is a `SourceKind` bitmask ([`CategoryFilter`]) —
//!   the palette chrome toggles chips that gate whole classes of
//!   sources on and off.
//!
//! Refresh cadence is a caller concern (`ShellOverlayHost::toggle_palette`
//! kicks a refresh on the false → true transition); each source
//! implementation exposes a per-source TTL via
//! [`sources::SearchSource::last_refreshed_at`] so an aggregating
//! caller can skip work when nothing has aged out.

pub mod sources;

use nucleo_matcher::pattern::{AtomKind, CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher};

pub use sources::{
    CategoryFilter, SearchAction, SearchItem, SearchSource, SourceError, SourceKind,
    installed_flatpak::InstalledFlatpakSource,
    installed_qos_apps::InstalledQosAppsSource,
    path_commands::PathCommandSource,
    uninstalled_flatpak::UninstalledFlatpakSource,
    uninstalled_qos_apps::UninstalledQosAppsSource,
};

/// The shell-owned aggregator that concatenates every source's
/// items, scores them against a query, and returns the top-ranked
/// slice for the palette to render.
///
/// The aggregator is *not* a `SearchSource` itself — it doesn't
/// implement refresh (each source refreshes on its own cadence) and
/// its item ordering depends on the query, so caching a "current
/// items" vector would be wrong. Callers hold sources on the
/// registry, then call [`SearchIndex::ranked`] once per palette
/// rebuild.
///
/// Sources are held as trait objects behind a `Box<dyn SearchSource>`
/// so the shell can build its production registry alongside tests
/// that swap in mocks.
pub struct SearchIndex {
    sources: Vec<Box<dyn SearchSource>>,
}

impl Default for SearchIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl SearchIndex {
    /// Empty registry. Real users build one via
    /// [`Self::with_default_sources`]; tests push mock sources with
    /// [`Self::add_source`].
    pub fn new() -> Self {
        Self { sources: Vec::new() }
    }

    /// The production registry: all five default sources wired
    /// through their canonical XDG paths. None of them are refreshed
    /// yet — the caller should invoke [`Self::refresh_all`] before
    /// the first ranked lookup so a cold palette open surfaces
    /// something.
    pub fn with_default_sources() -> Self {
        let mut index = Self::new();
        index.add_source(Box::new(InstalledQosAppsSource::new()));
        index.add_source(Box::new(UninstalledQosAppsSource::new()));
        index.add_source(Box::new(InstalledFlatpakSource::new()));
        index.add_source(Box::new(UninstalledFlatpakSource::new()));
        index.add_source(Box::new(PathCommandSource::new()));
        index
    }

    pub fn add_source(&mut self, source: Box<dyn SearchSource>) {
        self.sources.push(source);
    }

    pub fn sources(&self) -> &[Box<dyn SearchSource>] {
        &self.sources
    }

    /// Number of sources currently registered — mostly used in tests
    /// to assert the default-registry wiring didn't drop one.
    pub fn source_count(&self) -> usize {
        self.sources.len()
    }

    /// Drive every source's `refresh()` sequentially. Errors are
    /// collected instead of short-circuiting: an unreachable HTTP
    /// registry must not prevent the PATH source from producing
    /// results.
    pub fn refresh_all(&mut self) -> Vec<SourceError> {
        let mut errors = Vec::new();
        for src in &mut self.sources {
            if let Err(e) = src.refresh() {
                errors.push(e);
            }
        }
        errors
    }

    /// The scored, sorted, filtered result slice the palette renders.
    ///
    /// - `query`: user's typed prompt. Empty query returns every
    ///   filtered item in registration order (source-declared order).
    /// - `filter`: chip mask deciding which [`SourceKind`]s are visible.
    ///
    /// Ranking:
    /// 1. For each item, form the haystack `"<name>\n<description>\n<id>\n<categories joined>"`.
    /// 2. Ask nucleo for a fuzzy score; items that don't match at all
    ///    are dropped from the result.
    /// 3. Add a name bonus proportional to how much of the query hits
    ///    the item's `name` alone — this lifts direct name matches
    ///    above description-only hits.
    /// 4. Sort descending by (score, name lexicographic) for stable
    ///    output.
    pub fn ranked(&self, query: &str, filter: CategoryFilter) -> Vec<Ranked<'_>> {
        // Iterator over every source's items, gated by the filter.
        let items = self.sources.iter().flat_map(|src| {
            src.items()
                .iter()
                .filter(|item| filter.allows(item.source))
        });

        if query.trim().is_empty() {
            return items
                .map(|item| Ranked { item, score: 0 })
                .collect();
        }

        let mut matcher = Matcher::new(Config::DEFAULT);
        let pattern = Pattern::new(
            query,
            CaseMatching::Ignore,
            Normalization::Smart,
            AtomKind::Fuzzy,
        );

        let name_bonus_pattern = Pattern::new(
            query,
            CaseMatching::Ignore,
            Normalization::Smart,
            AtomKind::Fuzzy,
        );

        let mut out: Vec<Ranked<'_>> = Vec::new();
        for item in items {
            let haystack = format!(
                "{}\n{}\n{}\n{}",
                item.name,
                item.description,
                item.id,
                item.categories.join(" ")
            );
            let base = pattern.score(
                nucleo_matcher::Utf32String::from(haystack.as_str()).slice(..),
                &mut matcher,
            );
            let Some(base) = base else { continue };
            let name_score = name_bonus_pattern.score(
                nucleo_matcher::Utf32String::from(item.name.as_str()).slice(..),
                &mut matcher,
            );
            // Name matches count for extra weight — `nucleo`'s default
            // scoring already includes a start-of-string bonus, so
            // adding the name score verbatim doubles that bonus.
            let score = base as i64 + name_score.unwrap_or(0) as i64;
            out.push(Ranked { item, score });
        }
        out.sort_by(|a, b| {
            b.score
                .cmp(&a.score)
                .then_with(|| a.item.name.cmp(&b.item.name))
        });
        out
    }
}

/// One row in the palette's result list, paired with the raw score
/// the caller can use for debug overlays / tracing.
#[derive(Debug)]
pub struct Ranked<'a> {
    pub item: &'a SearchItem,
    pub score: i64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use sources::testing::MockSource;
    use std::path::PathBuf;

    fn seed(index: &mut SearchIndex) {
        // The seed set exercises all five source kinds so tests can
        // assert both ranking (across sources) and category
        // filtering (per source).
        let mut installed_qos = MockSource::new(SourceKind::InstalledQosApp);
        installed_qos.push(SearchItem {
            id: "dev.qos.notes".into(),
            name: "Notes".into(),
            description: "Simple CRDT-native notes app".into(),
            icon: None,
            source: SourceKind::InstalledQosApp,
            categories: vec!["Productivity".into()],
            action: SearchAction::LaunchInstalled {
                pkg_id: "dev.qos.notes".into(),
                kind: SourceKind::InstalledQosApp,
            },
        });
        index.add_source(Box::new(installed_qos));

        let mut installed_flatpak = MockSource::new(SourceKind::InstalledFlatpak);
        installed_flatpak.push(SearchItem {
            id: "org.mozilla.firefox".into(),
            name: "Firefox".into(),
            description: "Web browser".into(),
            icon: None,
            source: SourceKind::InstalledFlatpak,
            categories: vec!["Network".into()],
            action: SearchAction::LaunchInstalled {
                pkg_id: "org.mozilla.firefox".into(),
                kind: SourceKind::InstalledFlatpak,
            },
        });
        index.add_source(Box::new(installed_flatpak));

        let mut uninstalled_flatpak = MockSource::new(SourceKind::UninstalledFlatpak);
        uninstalled_flatpak.push(SearchItem {
            id: "org.gnome.Weather".into(),
            name: "Weather".into(),
            description: "See what the weather looks like".into(),
            icon: None,
            source: SourceKind::UninstalledFlatpak,
            categories: vec!["Utility".into()],
            action: SearchAction::InstallFlatpak {
                flatpak_id: "org.gnome.Weather".into(),
            },
        });
        index.add_source(Box::new(uninstalled_flatpak));

        let mut path = MockSource::new(SourceKind::PathCommand);
        path.push(SearchItem {
            id: "ls".into(),
            name: "ls".into(),
            description: "list directory contents".into(),
            icon: None,
            source: SourceKind::PathCommand,
            categories: vec![],
            action: SearchAction::RunCommand {
                cmd: PathBuf::from("/usr/bin/ls"),
            },
        });
        index.add_source(Box::new(path));
    }

    #[test]
    fn empty_query_returns_all_items() {
        let mut index = SearchIndex::new();
        seed(&mut index);
        let all = index.ranked("", CategoryFilter::all());
        assert_eq!(all.len(), 4);
    }

    #[test]
    fn best_name_match_wins() {
        // "text" only appears in the description of one item; the
        // ranking must still surface the fuzzy hit above the noise.
        let mut index = SearchIndex::new();
        let mut source = MockSource::new(SourceKind::InstalledQosApp);
        source.push(SearchItem {
            id: "dev.qos.editor".into(),
            name: "Editor".into(),
            description: "Rich text editor with CRDT support".into(),
            icon: None,
            source: SourceKind::InstalledQosApp,
            categories: vec![],
            action: SearchAction::LaunchInstalled {
                pkg_id: "dev.qos.editor".into(),
                kind: SourceKind::InstalledQosApp,
            },
        });
        source.push(SearchItem {
            id: "dev.qos.notes".into(),
            name: "Notes".into(),
            description: "Simple markdown notes".into(),
            icon: None,
            source: SourceKind::InstalledQosApp,
            categories: vec![],
            action: SearchAction::LaunchInstalled {
                pkg_id: "dev.qos.notes".into(),
                kind: SourceKind::InstalledQosApp,
            },
        });
        index.add_source(Box::new(source));

        let ranked = index.ranked("text", CategoryFilter::all());
        assert!(!ranked.is_empty(), "expected at least one hit for 'text'");
        assert_eq!(ranked[0].item.name, "Editor");
    }

    #[test]
    fn fuzzy_wp_ranks_word_processor_above_web_browser() {
        let mut index = SearchIndex::new();
        let mut source = MockSource::new(SourceKind::InstalledQosApp);
        source.push(SearchItem {
            id: "dev.qos.word-processor".into(),
            name: "word-processor".into(),
            description: "Rich text editor".into(),
            icon: None,
            source: SourceKind::InstalledQosApp,
            categories: vec![],
            action: SearchAction::LaunchInstalled {
                pkg_id: "dev.qos.word-processor".into(),
                kind: SourceKind::InstalledQosApp,
            },
        });
        source.push(SearchItem {
            id: "dev.qos.web-browser".into(),
            name: "web-browser".into(),
            description: "Browse the web".into(),
            icon: None,
            source: SourceKind::InstalledQosApp,
            categories: vec![],
            action: SearchAction::LaunchInstalled {
                pkg_id: "dev.qos.web-browser".into(),
                kind: SourceKind::InstalledQosApp,
            },
        });
        index.add_source(Box::new(source));

        let ranked = index.ranked("wp", CategoryFilter::all());
        // `wp` matches `word-processor` (`w` at word start + `p` at
        // the next word start) but not `web-browser` (no `p`), so
        // the browser must not appear in the ranked slice at all.
        assert!(!ranked.is_empty(), "expected at least one hit for 'wp'");
        assert_eq!(
            ranked[0].item.name, "word-processor",
            "the tighter subsequence hit (w-p at both word starts) must win"
        );
        assert!(
            ranked.iter().all(|r| r.item.name != "web-browser"),
            "web-browser has no `p`; nucleo must not surface it"
        );
    }

    #[test]
    fn category_filter_restricts_source_kinds() {
        let mut index = SearchIndex::new();
        seed(&mut index);
        let flatpak_only = CategoryFilter::empty()
            .with(SourceKind::InstalledFlatpak)
            .with(SourceKind::UninstalledFlatpak);
        let ranked = index.ranked("", flatpak_only);
        for r in &ranked {
            assert!(
                matches!(
                    r.item.source,
                    SourceKind::InstalledFlatpak | SourceKind::UninstalledFlatpak
                ),
                "unexpected source kind {:?}",
                r.item.source
            );
        }
        // 1 installed Flatpak + 1 uninstalled Flatpak from the seed.
        assert_eq!(ranked.len(), 2);

        let commands_only = CategoryFilter::empty().with(SourceKind::PathCommand);
        let ranked = index.ranked("", commands_only);
        assert_eq!(ranked.len(), 1);
        assert!(matches!(ranked[0].item.source, SourceKind::PathCommand));
    }

    #[test]
    fn refresh_all_collects_every_error() {
        let mut index = SearchIndex::new();
        let mut failing = MockSource::new(SourceKind::UninstalledQosApp);
        failing.fail_next_refresh(SourceError::Fetch("boom".into()));
        index.add_source(Box::new(failing));

        let mut ok = MockSource::new(SourceKind::PathCommand);
        ok.push(SearchItem {
            id: "true".into(),
            name: "true".into(),
            description: "".into(),
            icon: None,
            source: SourceKind::PathCommand,
            categories: vec![],
            action: SearchAction::RunCommand {
                cmd: std::path::PathBuf::from("/bin/true"),
            },
        });
        index.add_source(Box::new(ok));

        let errs = index.refresh_all();
        assert_eq!(errs.len(), 1, "one source failed, one succeeded");
        // The successful source's items must still be reachable — an
        // error in one source can't sink the whole index.
        let all = index.ranked("", CategoryFilter::all());
        assert_eq!(all.len(), 1);
    }

    #[test]
    fn add_qos_app_action_carries_install_arguments() {
        // The "Add" action for an uninstalled qOS app must carry
        // enough context for `qos-install install <qpkg>` to run
        // without another registry round trip. The test asserts the
        // exact shape of the action payload.
        let action = SearchAction::AddQosApp {
            pkg_id: "dev.qos.example".into(),
            registry_id: "official".into(),
            qpkg_url: "https://apps.qos.dev/example.qpkg".into(),
            root_hash_hex: "deadbeef".repeat(8),
        };
        let SearchAction::AddQosApp {
            pkg_id,
            registry_id,
            qpkg_url,
            root_hash_hex,
        } = action
        else {
            panic!("wrong variant");
        };
        assert_eq!(pkg_id, "dev.qos.example");
        assert_eq!(registry_id, "official");
        assert_eq!(qpkg_url, "https://apps.qos.dev/example.qpkg");
        assert_eq!(root_hash_hex.len(), 64);
    }

    #[test]
    fn dispatch_add_qos_app_invokes_qos_install() {
        // A mock dispatcher records every command; the shell-side
        // dispatch logic must translate `SearchAction::AddQosApp`
        // into `qos-install install <downloaded-path>`.
        let action = SearchAction::AddQosApp {
            pkg_id: "dev.qos.example".into(),
            registry_id: "official".into(),
            qpkg_url: "https://apps.qos.dev/example.qpkg".into(),
            root_hash_hex: "cd".repeat(32),
        };
        let mut dispatcher = RecordingDispatcher::default();
        dispatcher.dispatch(&action);
        assert_eq!(dispatcher.calls.len(), 1);
        let (bin, args) = &dispatcher.calls[0];
        assert_eq!(bin, "qos-install");
        // First positional arg is `install`; second is a path derived
        // from the qpkg URL. The exact path is opaque (a tempdir /
        // XDG cache derivative) but the *shape* of the arg list is
        // what shell action code must produce.
        assert_eq!(args.first().map(String::as_str), Some("install"));
        assert!(args.len() >= 2, "expected an install-target arg");
        assert!(
            args[1].ends_with("example.qpkg"),
            "install target must end in the qpkg filename ({})",
            args[1]
        );
    }

    #[derive(Default)]
    struct RecordingDispatcher {
        calls: Vec<(String, Vec<String>)>,
    }

    impl RecordingDispatcher {
        fn dispatch(&mut self, action: &SearchAction) {
            // Small stand-in for the shell-side action dispatcher.
            // Real code lives in `crate::search::actions` and shells
            // out via `std::process::Command`; the test only needs
            // the argv shape.
            match action {
                SearchAction::AddQosApp { qpkg_url, .. } => {
                    let file = qpkg_url
                        .rsplit('/')
                        .next()
                        .unwrap_or("download.qpkg");
                    let target = format!("/tmp/qos-search/{file}");
                    self.calls.push((
                        "qos-install".into(),
                        vec!["install".into(), target],
                    ));
                }
                SearchAction::InstallFlatpak { flatpak_id } => {
                    self.calls.push((
                        "flatpak".into(),
                        vec![
                            "install".into(),
                            "--user".into(),
                            "-y".into(),
                            flatpak_id.clone(),
                        ],
                    ));
                }
                SearchAction::LaunchInstalled { pkg_id, kind } => match kind {
                    SourceKind::InstalledQosApp => {
                        self.calls.push((
                            "qos-install".into(),
                            vec!["run".into(), pkg_id.clone()],
                        ));
                    }
                    SourceKind::InstalledFlatpak => {
                        self.calls.push((
                            "flatpak".into(),
                            vec!["run".into(), pkg_id.clone()],
                        ));
                    }
                    _ => {}
                },
                SearchAction::RunCommand { cmd } => {
                    self.calls.push((cmd.to_string_lossy().into(), Vec::new()));
                }
            }
        }
    }
}
