//! Host applications provider: everything already installed on the machine
//! that Tilo did not package.
//!
//! Deliberately a separate provider under a separate [`ResourceKind`]
//! rather than more rows under [`ApplicationsProvider`]: a Tilo app runs
//! sandboxed with one writable directory, a host app runs unconfined with
//! the user's own files. Own chip ("Host Apps"), own group header, and a
//! row that says "unconfined" outright.
//!
//! [`ApplicationsProvider`]: super::applications::ApplicationsProvider

use crate::shell::chrome_state::Chrome;
use crate::shell::host_apps::{self, HostApp};
use crate::shell::notification::{Notification, NotificationSeverity};
use crate::shell::search::{
    ResourceKind, SearchBudget, SearchContext, SearchCursor, SearchPage, SearchProvider,
    SearchResult,
};
use std::cell::RefCell;
use std::collections::HashSet;
use std::sync::mpsc::{self, Receiver, TryRecvError};
#[cfg(test)]
use std::sync::{Arc, Mutex};

/// Chip label / group header, mirroring [`ResourceKind::HostApplication`].
pub const HOST_APPLICATIONS_KIND: &str = "Host Apps";

pub struct HostApplicationsProvider {
    kinds: Vec<ResourceKind>,
    snapshot: RefCell<Option<HostAppsSnapshot>>,
    build: RefCell<Option<Receiver<HostAppsSnapshot>>>,
    #[cfg(test)]
    source: Option<Arc<HostAppsTestSource>>,
    #[cfg(test)]
    build_probe: Arc<BuildProbe>,
}

struct HostAppsSnapshot {
    source_generation: (u64, u64),
    apps: Vec<HostApp>,
    installed_ids: HashSet<String>,
    index: ui::search::SearchTextIndex,
}

#[cfg(test)]
#[derive(Default)]
struct BuildProbe {
    builds: std::sync::atomic::AtomicUsize,
    thread: Mutex<Option<std::thread::ThreadId>>,
}

#[cfg(test)]
struct HostAppsTestSource {
    generation: std::sync::atomic::AtomicU64,
    state: Mutex<(Vec<HostApp>, Vec<crate::shell::launcher::InstalledApp>)>,
    materializations: std::sync::atomic::AtomicUsize,
}

#[cfg(test)]
impl HostAppsTestSource {
    fn new(apps: Vec<HostApp>, installed: Vec<crate::shell::launcher::InstalledApp>) -> Arc<Self> {
        Arc::new(Self {
            generation: std::sync::atomic::AtomicU64::new(1),
            state: Mutex::new((apps, installed)),
            materializations: std::sync::atomic::AtomicUsize::new(0),
        })
    }

    fn generation(&self) -> (u64, u64) {
        let generation = self.generation.load(std::sync::atomic::Ordering::Acquire);
        (generation, generation)
    }

    fn snapshot(&self) -> (u64, Vec<HostApp>, Vec<crate::shell::launcher::InstalledApp>) {
        self.materializations
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let generation = self.generation.load(std::sync::atomic::Ordering::Acquire);
        let (apps, installed) = self
            .state
            .lock()
            .expect("host app test source lock")
            .clone();
        (generation, apps, installed)
    }
}

impl HostApplicationsProvider {
    pub fn new() -> Self {
        Self {
            kinds: vec![ResourceKind::HostApplication],
            snapshot: RefCell::new(None),
            build: RefCell::new(None),
            #[cfg(test)]
            source: None,
            #[cfg(test)]
            build_probe: Arc::new(BuildProbe::default()),
        }
    }

    #[cfg(test)]
    fn with_test_source(source: Arc<HostAppsTestSource>) -> Self {
        Self {
            source: Some(source),
            ..Self::new()
        }
    }

    #[allow(clippy::unused_self, reason = "test builds use the injected source")]
    fn source_generation(&self) -> (u64, u64) {
        #[cfg(test)]
        if let Some(source) = &self.source {
            return source.generation();
        }
        (
            host_apps::host_apps_generation(),
            crate::shell::launcher::gui_apps_generation(),
        )
    }

    /// Build the expensive full host-desktop projection while the registry is
    /// refreshing sources, never from `query_page` itself.
    fn build_snapshot(
        #[cfg(test)] source: Option<Arc<HostAppsTestSource>>,
        #[cfg(test)] probe: &BuildProbe,
    ) -> HostAppsSnapshot {
        #[cfg(test)]
        {
            probe
                .builds
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            *probe.thread.lock().expect("host app build thread lock") =
                Some(std::thread::current().id());
        }
        #[cfg(test)]
        if let Some(source) = source {
            let (generation, apps, installed) = source.snapshot();
            return Self::snapshot_from_parts((generation, generation), apps, &installed);
        }
        let (host_generation, apps) = host_apps::host_apps_snapshot();
        let (installed_generation, installed) = crate::shell::launcher::gui_apps_snapshot();
        Self::snapshot_from_parts((host_generation, installed_generation), apps, &installed)
    }

    fn snapshot_from_parts(
        source_generation: (u64, u64),
        apps: Vec<HostApp>,
        installed: &[crate::shell::launcher::InstalledApp],
    ) -> HostAppsSnapshot {
        let installed_ids = installed
            .iter()
            .map(|app| app.package_id.to_ascii_lowercase())
            .collect();
        let index = ui::search::SearchTextIndex::new(apps.iter().map(|app| {
            format!(
                "{}\u{1f}{}\u{1f}{}\u{1f}{}",
                app.name,
                app.desktop_id,
                app.keywords.join(" "),
                app.categories.join(" ")
            )
        }));
        HostAppsSnapshot {
            source_generation,
            apps,
            installed_ids,
            index,
        }
    }

    fn refresh_snapshot(&self) {
        let receiver = self.build.borrow_mut().take();
        if let Some(receiver) = receiver {
            match receiver.try_recv() {
                Ok(snapshot) => *self.snapshot.borrow_mut() = Some(snapshot),
                Err(TryRecvError::Empty) => *self.build.borrow_mut() = Some(receiver),
                Err(TryRecvError::Disconnected) => {}
            }
        }
        let current = self.source_generation();
        if self
            .snapshot
            .borrow()
            .as_ref()
            .is_some_and(|snapshot| snapshot.source_generation == current)
            || self.build.borrow().is_some()
        {
            return;
        }
        let (sender, receiver) = mpsc::sync_channel(1);
        if std::thread::Builder::new()
            .name("tilo-host-search-index".into())
            .spawn({
                #[cfg(test)]
                let source = self.source.clone();
                #[cfg(test)]
                let probe = Arc::clone(&self.build_probe);
                move || {
                    let _ = sender.send(Self::build_snapshot(
                        #[cfg(test)]
                        source,
                        #[cfg(test)]
                        &probe,
                    ));
                }
            })
            .is_ok()
        {
            *self.build.borrow_mut() = Some(receiver);
        }
    }

    fn snapshot(&self) -> Option<std::cell::Ref<'_, HostAppsSnapshot>> {
        let generation = self.source_generation();
        let snapshot = self.snapshot.borrow();
        std::cell::Ref::filter_map(snapshot, Option::as_ref)
            .ok()
            .filter(|snapshot| snapshot.source_generation == generation)
    }

    /// [`Self::results_for`] with the installed-package list supplied, so a
    /// test's scoring doesn't depend on what happens to be installed.
    #[cfg(test)]
    fn results_against(
        &self,
        needle: &str,
        apps: Vec<HostApp>,
        installed: &[crate::shell::launcher::InstalledApp],
    ) -> Vec<SearchResult> {
        let installed_ids = installed
            .iter()
            .map(|app| app.package_id.to_ascii_lowercase())
            .collect();
        self.results_against_membership(needle, apps, &installed_ids)
    }

    fn results_against_membership(
        &self,
        needle: &str,
        apps: Vec<HostApp>,
        installed_ids: &HashSet<String>,
    ) -> Vec<SearchResult> {
        let needle_lower = needle.to_lowercase();
        let mut out = Vec::new();
        for app in apps {
            if installed_ids.contains(&app.desktop_id.to_ascii_lowercase()) {
                continue;
            }
            let name_lower = app.name.to_lowercase();
            let id_lower = app.desktop_id.to_lowercase();
            let Some(score) = match_score(&needle_lower, &name_lower, &id_lower, &app) else {
                continue;
            };
            let desktop_id = app.desktop_id.clone();
            let display_name = app.name.clone();
            let notify_name = display_name.clone();
            out.push(SearchResult::nullary(
                ResourceKind::HostApplication,
                self.id(),
                app.desktop_id.clone(),
                format!("Open {}", app.name),
                // "Unconfined" is not decoration: this app is about to get
                // the user's real home directory.
                Some(format!(
                    "{} · unconfined host app · {}",
                    app.source_label, app.desktop_id
                )),
                score,
                move |_doc, _root, chrome: &Chrome| {
                    // Decided now, while the layout is still true; a
                    // foreign toplevel announces an app-id Tilo did not
                    // mint, so the note is keyed on every declared identity.
                    let result = if let Some(app) = host_apps::find(&desktop_id) {
                        let st = chrome.borrow();
                        let tree = st.tree();
                        let placement =
                            crate::shell::placement::note_placement_for_launch_identities(
                                &app.desktop_id,
                                &app.window_identities,
                                &tree,
                                st.strip_viewport(),
                            );
                        let portal_workspace_id = tree
                            .active_workspace()
                            .map(|workspace| workspace.room.to_string());
                        drop(tree);
                        drop(st);
                        host_apps::queue_launch(host_apps::QueuedLaunch {
                            desktop_id: app.desktop_id,
                            display_name: app.name,
                            placement,
                            portal_workspace_id,
                        })
                    } else {
                        host_apps::launch(&desktop_id, None).map(|_| ())
                    };
                    if let Err(err) = result {
                        crate::shell::launcher::clear_pending_placement(&desktop_id);
                        tracing::error!(
                            target: "tilo::app_launch",
                            desktop_id = %desktop_id,
                            display_name = %notify_name,
                            detail = %err,
                            "host application launch was refused before spawn"
                        );
                        let mut st = chrome.borrow_mut();
                        st.push_notification_full(
                            Notification::new(
                                crate::shell::launcher::launch_failure_title(&notify_name),
                                err.notification_message(),
                            )
                            .with_severity(NotificationSeverity::Error)
                            .with_app_name(HOST_APPLICATIONS_KIND)
                            .with_dismiss_after(
                                crate::shell::launcher::LAUNCH_FAILURE_NOTICE_LIFETIME,
                            ),
                        );
                        st.bump();
                    }
                },
            ));
        }
        out
    }
}

/// Whether an installed Tilo package already answers for this app. Names and
/// reverse-DNS tails are deliberately insufficient: different desktops ship
/// real, distinct applications called Calendar, Notes and Terminal.
#[cfg(test)]
fn shadowed_by_a_tilo_package(
    app: &HostApp,
    installed: &[crate::shell::launcher::InstalledApp],
) -> bool {
    installed
        .iter()
        .any(|pkg| pkg.package_id.eq_ignore_ascii_case(&app.desktop_id))
}

/// Same scale as the applications provider, one step lower at each rung so
/// a confined Tilo app sorts first at an equal match. `Keywords=` and
/// categories are matched too — how typing "browser" finds Firefox.
fn match_score(needle: &str, name: &str, desktop_id: &str, app: &HostApp) -> Option<i32> {
    if needle.is_empty() {
        return Some(0);
    }
    if name.starts_with(needle) {
        return Some(95);
    }
    // The full id, or its last segment — `org.gnome.TextEditor` named
    // either way identifies exactly one application.
    if desktop_id == needle || desktop_id.rsplit('.').next() == Some(needle) {
        return Some(95);
    }
    if name.contains(needle) {
        return Some(45);
    }
    if desktop_id.starts_with(needle) {
        return Some(55);
    }
    if desktop_id.contains(needle) {
        return Some(24);
    }
    if app
        .keywords
        .iter()
        .any(|k| k.to_lowercase().contains(needle))
    {
        return Some(22);
    }
    if app
        .categories
        .iter()
        .any(|c| c.to_lowercase().contains(needle))
    {
        return Some(19);
    }
    if fuzzy_subsequence(needle, name) {
        return Some(9);
    }
    None
}

fn fuzzy_subsequence(needle: &str, hay: &str) -> bool {
    let mut hi = hay.chars();
    'ch: for nc in needle.chars() {
        for hc in hi.by_ref() {
            if hc == nc {
                continue 'ch;
            }
        }
        return false;
    }
    true
}

impl Default for HostApplicationsProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl SearchProvider for HostApplicationsProvider {
    fn id(&self) -> &'static str {
        "host-applications"
    }
    fn name(&self) -> &str {
        HOST_APPLICATIONS_KIND
    }
    fn resource_kinds(&self) -> &[ResourceKind] {
        &self.kinds
    }
    fn query(&self, ctx: SearchContext<'_>) -> Vec<SearchResult> {
        if !ctx
            .wanted
            .iter()
            .any(|k| matches!(k, ResourceKind::HostApplication))
        {
            return Vec::new();
        }
        // Compatibility for callers outside the registry. Palette paging
        // refreshes retained indexes before it calls into a provider.
        let _ = self.search_revision(&ctx);
        self.query_page(ctx, None, SearchBudget::PALETTE).results
    }

    fn refresh_search_index(&self, _ctx: &SearchContext<'_>) {
        self.refresh_snapshot();
    }

    fn search_revision(&self, _ctx: &SearchContext<'_>) -> String {
        let (host, installed) = self.source_generation();
        let ready = self
            .snapshot
            .borrow()
            .as_ref()
            .is_some_and(|snapshot| snapshot.source_generation == (host, installed));
        format!(
            "{}:{host}:{installed}",
            if ready { "ready" } else { "indexing" }
        )
    }

    fn query_page(
        &self,
        ctx: SearchContext<'_>,
        cursor: Option<&SearchCursor>,
        budget: SearchBudget,
    ) -> SearchPage {
        if !ctx.wanted.contains(&ResourceKind::HostApplication)
            || budget.max_results == 0
            || budget.max_work == 0
        {
            return SearchPage::exhausted(self.search_revision(&ctx), Vec::new());
        }
        let revision = self.search_revision(&ctx);
        let Some(snapshot) = self.snapshot() else {
            return SearchPage::exhausted(revision, Vec::new());
        };
        let after = match cursor {
            Some(cursor) if cursor.revision != revision => return SearchPage::stale(revision),
            Some(cursor) => match cursor.token.strip_prefix("host:").and_then(|token| {
                token
                    .split_once('\u{1f}')
                    .filter(|(_, query)| *query == ctx.needle)
                    .and_then(|(offset, _)| offset.parse::<usize>().ok())
            }) {
                Some(offset) => Some(offset),
                None => return SearchPage::stale(revision),
            },
            None => None,
        };
        let page =
            snapshot
                .index
                .matching_page(ctx.needle, after, budget.max_work, budget.max_results);
        let mut next_after = page.next_after;
        let mut bytes = 0usize;
        let mut results = Vec::new();
        for id in page.ids {
            let Some(app) = snapshot.apps.get(id) else {
                continue;
            };
            let mut rows = self.results_against_membership(
                ctx.needle,
                vec![app.clone()],
                &snapshot.installed_ids,
            );
            let Some(row) = rows.pop() else { continue };
            let row_bytes = row
                .title
                .len()
                .saturating_add(row.subtitle.as_ref().map_or(0, String::len));
            if bytes.saturating_add(row_bytes) > budget.max_bytes {
                // Skip this one too-large row once, and continue after the
                // candidate actually considered rather than the index page's
                // farther endpoint.
                next_after = Some(id);
                break;
            }
            bytes = bytes.saturating_add(row_bytes);
            results.push(row);
        }
        let exhausted = next_after.is_none();
        SearchPage {
            revision: revision.clone(),
            results,
            next_cursor: next_after.map(|offset| SearchCursor {
                revision,
                token: format!("host:{offset}\u{1f}{}", ctx.needle),
            }),
            exhausted,
            stale_cursor: false,
            budget_exhausted: !exhausted,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::chrome_state::ChromeState;
    use crate::shell::state::{WorkspaceTree, WorkspaceTreeHandle};
    use automerge::AutoCommit;
    use parking_lot::RwLock;
    use std::sync::Arc;

    fn chrome_rc() -> Chrome {
        let handle: WorkspaceTreeHandle = Arc::new(RwLock::new(WorkspaceTree::default()));
        ChromeState::new_with_handle(handle)
    }

    fn app(desktop_id: &str, name: &str) -> HostApp {
        HostApp {
            desktop_id: desktop_id.to_string(),
            name: name.to_string(),
            source_label: "Flatpak",
            categories: Vec::new(),
            keywords: Vec::new(),
            window_identities: vec![desktop_id.to_string()],
        }
    }

    #[test]
    fn a_matching_host_app_offers_an_open_row() {
        let p = HostApplicationsProvider::new();
        let out = p.results_against("spot", vec![app("com.spotify.Client", "Spotify")], &[]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].title, "Open Spotify");
        assert_eq!(out[0].id, "com.spotify.Client");
        assert_eq!(out[0].kind, ResourceKind::HostApplication);
    }

    #[test]
    fn cold_and_warm_public_pages_never_rescan_a_hundred_thousand_host_apps() {
        let chrome = chrome_rc();
        let chrome = chrome.borrow();
        let doc = AutoCommit::new();
        let ctx = SearchContext {
            needle: "unique-tail",
            wanted: &[ResourceKind::HostApplication],
            chrome: &chrome,
            doc: &doc,
            doc_root: &automerge::ROOT,
        };
        let cold = HostApplicationsProvider::new();
        assert!(
            cold.query_page(ctx, None, SearchBudget::PALETTE)
                .results
                .is_empty()
        );
        assert!(
            cold.build.borrow().is_none(),
            "a public cold query must not start or clone a full host-app corpus"
        );

        let provider = HostApplicationsProvider::new();
        let apps = (0..100_001)
            .map(|index| {
                app(
                    &format!("org.fixture.App{index}"),
                    if index == 100_000 {
                        "unique-tail"
                    } else {
                        "common app"
                    },
                )
            })
            .collect::<Vec<_>>();
        let index = ui::search::SearchTextIndex::new(
            apps.iter()
                .map(|app| format!("{}\u{1f}{}", app.name, app.desktop_id)),
        );
        *provider.snapshot.borrow_mut() = Some(HostAppsSnapshot {
            source_generation: provider.source_generation(),
            apps,
            installed_ids: HashSet::new(),
            index,
        });
        let page = provider.query_page(
            SearchContext {
                needle: "unique-tail",
                wanted: &[ResourceKind::HostApplication],
                chrome: &chrome,
                doc: &doc,
                doc_root: &automerge::ROOT,
            },
            None,
            SearchBudget {
                max_results: 1,
                max_bytes: usize::MAX,
                max_work: 1,
            },
        );
        assert_eq!(page.results.len(), 1);
        assert_eq!(page.results[0].title, "Open unique-tail");
        assert!(page.exhausted, "the rare posting needs one warm inspection");
        assert!(provider.build.borrow().is_none());
    }

    #[test]
    fn source_backed_cold_query_does_no_host_index_work_before_refresh() {
        let source = HostAppsTestSource::new(
            (0..100_001)
                .map(|index| {
                    app(
                        &format!("org.fixture.Source{index}"),
                        if index == 100_000 {
                            "unique-source-tail"
                        } else {
                            "common source app"
                        },
                    )
                })
                .collect(),
            Vec::new(),
        );
        let provider = HostApplicationsProvider::with_test_source(Arc::clone(&source));
        let chrome = chrome_rc();
        let chrome = chrome.borrow();
        let doc = AutoCommit::new();
        let ctx = SearchContext {
            needle: "unique-source-tail",
            wanted: &[ResourceKind::HostApplication],
            chrome: &chrome,
            doc: &doc,
            doc_root: &automerge::ROOT,
        };
        let query_thread = std::thread::current().id();
        assert!(provider.search_revision(&ctx).starts_with("indexing:"));
        assert!(
            provider
                .query_page(ctx, None, SearchBudget::PALETTE)
                .results
                .is_empty()
        );
        assert_eq!(
            provider
                .build_probe
                .builds
                .load(std::sync::atomic::Ordering::Relaxed),
            0
        );
        assert_eq!(
            source
                .materializations
                .load(std::sync::atomic::Ordering::Relaxed),
            0
        );
        assert!(provider.build.borrow().is_none());
        for _ in 0..6_000 {
            provider.refresh_search_index(&ctx);
            if provider.search_revision(&ctx).starts_with("ready:") {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(provider.search_revision(&ctx).starts_with("ready:"));
        assert_eq!(
            provider
                .build_probe
                .builds
                .load(std::sync::atomic::Ordering::Relaxed),
            1
        );
        assert_ne!(
            *provider
                .build_probe
                .thread
                .lock()
                .expect("host app build thread lock"),
            Some(query_thread)
        );
        let page = provider.query_page(
            SearchContext { ..ctx },
            None,
            SearchBudget {
                max_results: 1,
                max_bytes: usize::MAX,
                max_work: 1,
            },
        );
        assert_eq!(page.results[0].title, "Open unique-source-tail");
    }

    /// "Unconfined" in the subtitle is the whole point of the separate kind.
    #[test]
    fn the_row_names_the_source_and_says_it_is_unconfined() {
        let p = HostApplicationsProvider::new();
        let out = p.results_against("spot", vec![app("com.spotify.Client", "Spotify")], &[]);
        let subtitle = out[0].subtitle.as_deref().expect("a subtitle");
        assert!(subtitle.contains("Flatpak"), "{subtitle}");
        assert!(
            subtitle.contains("unconfined"),
            "the user must be able to tell a confined Tilo app from an \
             unconfined host app without opening anything: {subtitle}"
        );
        assert!(subtitle.contains("com.spotify.Client"), "{subtitle}");
    }

    #[test]
    fn a_system_app_row_names_system_provenance() {
        let p = HostApplicationsProvider::new();
        let system = HostApp {
            source_label: "System",
            ..app("org.gnome.Console", "Terminal")
        };
        let out = p.results_against("terminal", vec![system], &[]);
        let subtitle = out[0].subtitle.as_deref().expect("a subtitle");
        assert!(subtitle.starts_with("System · unconfined host app ·"));
    }

    #[test]
    fn host_apps_are_a_separate_kind_from_tilo_applications() {
        let p = HostApplicationsProvider::new();
        assert_eq!(p.resource_kinds(), &[ResourceKind::HostApplication]);
        assert_eq!(ResourceKind::HostApplication.label(), "Host Apps");
        assert_ne!(
            ResourceKind::HostApplication.label(),
            ResourceKind::Application.label()
        );
    }

    /// Equal-quality matches rank the confined app first — Enter takes the top row.
    #[test]
    fn a_host_app_scores_below_an_equally_good_tilo_match() {
        use crate::shell::search::providers::applications::ApplicationsProvider;
        let host = HostApplicationsProvider::new();
        let tilo = ApplicationsProvider::new();
        // Different name/id tail, so this measures the scale, not shadowing.
        let host_rows =
            host.results_against("term", vec![app("org.x.Terminator", "Terminator")], &[]);
        let tilo_rows = tilo.results_for(
            "term",
            vec![crate::shell::launcher::InstalledApp {
                package_id: "so.tilo.terminal".into(),
                display_name: "Terminal".into(),
                category: None,
                version: "1".into(),
                manifest_path: std::path::PathBuf::from("/x/manifest.toml"),
                payload_root: std::path::PathBuf::from("/x"),
                has_gui: true,
                opens_extensions: Vec::new(),
                opens_mime_types: Vec::new(),
                opens_uri_schemes: Vec::new(),
            }],
            crate::shell::chrome_state::OpenTarget::Beside,
        );
        assert!(
            tilo_rows[0].score > host_rows[0].score,
            "tilo {} must outrank host {}",
            tilo_rows[0].score,
            host_rows[0].score
        );
    }

    #[test]
    fn keywords_and_categories_are_matchable() {
        let p = HostApplicationsProvider::new();
        let mut firefox = app("org.mozilla.firefox", "Firefox");
        firefox.keywords = vec!["Internet".into(), "WWW".into()];
        firefox.categories = vec!["Network".into(), "WebBrowser".into()];
        let by_keyword = p.results_against("internet", vec![firefox.clone()], &[]);
        assert_eq!(by_keyword.len(), 1, "{by_keyword:?}");
        let by_category = p.results_against("webbrowser", vec![firefox], &[]);
        assert_eq!(by_category.len(), 1, "{by_category:?}");
        assert!(
            by_keyword[0].score > by_category[0].score,
            "a declared keyword is a stronger signal than a category bucket"
        );
    }

    #[test]
    fn an_empty_needle_lists_every_host_app() {
        let p = HostApplicationsProvider::new();
        let out = p.results_against("", vec![app("a.b.One", "One"), app("a.b.Two", "Two")], &[]);
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|r| r.score == 0));
    }

    #[test]
    fn a_non_matching_needle_returns_nothing() {
        let p = HostApplicationsProvider::new();
        assert!(
            p.results_against("zzzqqq", vec![app("com.spotify.Client", "Spotify")], &[])
                .is_empty()
        );
    }

    #[test]
    fn unrequested_kind_returns_empty() {
        let p = HostApplicationsProvider::new();
        let c = chrome_rc();
        let cs = c.borrow();
        let doc = AutoCommit::new();
        let out = p.query(SearchContext {
            needle: "spotify",
            wanted: &[ResourceKind::Application],
            chrome: &cs,
            doc: &doc,
            doc_root: &automerge::ROOT,
        });
        assert!(
            out.is_empty(),
            "the Applications chip must not pull in host apps — that is \
             the distinction the separate kind exists to draw"
        );
    }

    /// Must surface a visible error, not fail silently. Drives the
    /// `on_open` closure.
    #[test]
    fn a_failed_launch_pushes_an_error_notification() {
        let p = HostApplicationsProvider::new();
        let c = chrome_rc();
        let mut doc = AutoCommit::new();
        let out = p.results_against("ghost", vec![app("so.tilo.ghost-host-app", "Ghost")], &[]);
        let row = out.first().expect("a row for the ghost app");
        assert!(c.borrow().notifications().is_empty());
        (row.on_open)(
            &mut doc,
            &automerge::ROOT,
            &c,
            &crate::shell::search::action_api::ActionArgs::none(),
        )
        .expect("launching takes no arguments");
        let st = c.borrow();
        let n = st
            .notifications()
            .last()
            .expect("a failed launch must notify the user");
        assert_eq!(n.title, "Couldn't open Ghost");
        assert_eq!(n.severity, NotificationSeverity::Error);
        assert!(!n.body.is_empty());
    }

    fn installed(package_id: &str, display_name: &str) -> crate::shell::launcher::InstalledApp {
        crate::shell::launcher::InstalledApp {
            package_id: package_id.into(),
            display_name: display_name.into(),
            category: None,
            version: "1".into(),
            manifest_path: std::path::PathBuf::from("/x/manifest.toml"),
            payload_root: std::path::PathBuf::from("/x"),
            has_gui: true,
            opens_extensions: Vec::new(),
            opens_mime_types: Vec::new(),
            opens_uri_schemes: Vec::new(),
        }
    }

    #[test]
    fn only_an_exact_tilo_package_identity_hides_the_host_row() {
        let installed = vec![installed("so.tilo.terminal", "Terminal")];
        assert!(shadowed_by_a_tilo_package(
            &app("so.tilo.terminal", "Terminal"),
            &installed
        ));
    }

    #[test]
    fn tilo_terminal_does_not_hide_a_distinct_host_terminal() {
        let installed = vec![installed("so.tilo.terminal", "Terminal")];
        assert!(!shadowed_by_a_tilo_package(
            &app("org.gnome.Console", "Terminal"),
            &installed
        ));
    }

    #[test]
    fn tilo_calendar_does_not_hide_gnome_calendar() {
        let installed = vec![installed("so.tilo.calendar", "Calendar")];
        assert!(!shadowed_by_a_tilo_package(
            &app("org.gnome.Calendar", "Calendar"),
            &installed
        ));
    }

    #[test]
    fn a_generic_shared_name_does_not_hide_a_host_app() {
        let installed = vec![installed("so.tilo.notes", "Notes")];
        assert!(!shadowed_by_a_tilo_package(
            &app("org.example.PrivateNotes", "Notes"),
            &installed
        ));
    }
}
