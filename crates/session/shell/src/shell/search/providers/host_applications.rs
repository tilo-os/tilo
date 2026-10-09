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
use crate::shell::i18n::{t, t_args};
use crate::shell::search::{
    ResourceKind, SearchBudget, SearchContext, SearchCursor, SearchPage, SearchProvider,
    SearchResult,
};
use std::cell::RefCell;
use std::collections::HashSet;
use std::sync::mpsc::{self, Receiver, TryRecvError};
#[cfg(test)]
use std::sync::{Arc, Mutex};
use tilo_desktop::notifications::{Notification, NotificationSeverity};
use tilo_search::{Document, FieldWeight, Matching, Page, SearchIndex};

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
    /// Lower-cased ids of installed Tilo packages; a host app with one of
    /// these ids is answered by the package instead.
    installed_ids: HashSet<String>,
    /// Apps no Tilo package answers for, keyed by position in `apps`.
    index: SearchIndex,
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
        let installed_ids: HashSet<String> = installed
            .iter()
            .map(|app| app.package_id.to_ascii_lowercase())
            .collect();
        let index = index_apps(&apps, &installed_ids);
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

    /// Wait, at most `bound`, for the index build [`Self::refresh_snapshot`]
    /// starts, so an address resolves on a session's first request too.
    fn settle_snapshot(&self, bound: std::time::Duration) {
        let started = std::time::Instant::now();
        loop {
            self.refresh_snapshot();
            if self.snapshot().is_some() {
                return;
            }
            let Some(receiver) = self.build.borrow_mut().take() else {
                return;
            };
            match receiver.recv_timeout(bound.saturating_sub(started.elapsed())) {
                Ok(snapshot) => *self.snapshot.borrow_mut() = Some(snapshot),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    *self.build.borrow_mut() = Some(receiver);
                    return;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            }
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
        apps: &[HostApp],
        installed: &[crate::shell::launcher::InstalledApp],
    ) -> Vec<SearchResult> {
        let installed_ids = installed
            .iter()
            .map(|app| app.package_id.to_ascii_lowercase())
            .collect();
        let index = index_apps(apps, &installed_ids);
        index
            .search(needle, Page::first(apps.len()))
            .hits
            .iter()
            .filter_map(|hit| {
                let app = apps.get(usize::try_from(hit.key).ok()?)?;
                Some(self.row(app, score_for(needle, hit.rank)))
            })
            .collect()
    }

    /// The open row for `app`.
    fn row(&self, app: &HostApp, score: i32) -> SearchResult {
        let desktop_id = app.desktop_id.clone();
        let notify_name = app.name.clone();
        SearchResult::nullary(
            ResourceKind::HostApplication,
            self.id(),
            app.desktop_id.clone(),
            t_args("search-host-app-open", &[("app", app.name.as_str().into())]),
            // "Unconfined" is not decoration: this app is about to get
            // the user's real home directory.
            Some(t_args(
                "search-host-app-subtitle",
                &[
                    ("source", app.source_display().into()),
                    ("id", app.desktop_id.as_str().into()),
                ],
            )),
            score,
            move |_doc, _root, chrome: &Chrome| {
                // Decided now, while the layout is still true; a
                // foreign toplevel announces an app-id Tilo did not
                // mint, so the note is keyed on every declared identity.
                let result = if let Some(app) = host_apps::find(&desktop_id) {
                    let st = chrome.borrow();
                    let tree = st.tree();
                    let placement = crate::shell::placement::note_placement_for_launch_identities(
                        &app.desktop_id,
                        &app.window_identities,
                        &tree,
                        st.strip_viewport(),
                    );
                    let portal_workspace_id = tree
                        .desktop
                        .active_workspace()
                        .map(|workspace| workspace.room.to_string());
                    drop(tree);
                    drop(st);
                    host_apps::queue_launch(host_apps::QueuedLaunch {
                        app,
                        placement,
                        portal_workspace_id,
                        requested_at: std::time::Instant::now(),
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
                        .with_dismiss_after(crate::shell::launcher::LAUNCH_FAILURE_NOTICE_LIFETIME),
                    );
                    st.bump();
                }
            },
        )
    }
}

/// Index the apps no installed Tilo package shadows, keyed by position.
/// `Keywords=` and categories are matched too: how typing "browser" finds
/// Firefox.
fn index_apps(apps: &[HostApp], installed_ids: &HashSet<String>) -> SearchIndex {
    let mut index = SearchIndex::new(Matching::LOOSE);
    index.replace_all(apps.iter().enumerate().filter_map(|(at, app)| {
        if installed_ids.contains(&app.desktop_id.to_ascii_lowercase()) {
            return None;
        }
        let desktop_id = app.desktop_id.as_str();
        let last_segment = desktop_id.rsplit('.').next().unwrap_or(desktop_id);
        Some(
            Document::new(at as u64)
                .field(app.name.as_str(), FieldWeight::TITLE)
                .field(last_segment, FieldWeight::TITLE)
                .field(desktop_id, super::applications::PACKAGE_ID_WEIGHT)
                .field(app.keywords.join(" "), FieldWeight::METADATA)
                .field(app.categories.join(" "), FieldWeight::METADATA),
        )
    }));
    if let Err(error) = index.commit() {
        tracing::error!(%error, "could not index host applications");
    }
    index
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

/// Ranks lower than a Tilo app at an equal match, so a confined Tilo app
/// sorts first.
const HOST_APP_STEP: i32 = 5;

/// The applications provider's scale, one step lower so a confined Tilo
/// app sorts first at an equal match. An empty needle lists every app at 0.
fn score_for(needle: &str, rank: Option<i32>) -> i32 {
    if needle.trim().is_empty() {
        return 0;
    }
    crate::shell::search::provider_score(rank.unwrap_or(0))
        .saturating_sub(HOST_APP_STEP)
        .max(1)
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
    fn display_name(&self) -> String {
        t("search-provider-host-apps")
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

    fn query_incomplete_reason(
        &self,
        _ctx: &SearchContext<'_>,
    ) -> Option<crate::shell::search::QueryIncompleteReason> {
        if self.build.borrow().is_some() {
            Some(crate::shell::search::QueryIncompleteReason::Pending)
        } else if self.snapshot().is_none() {
            Some(crate::shell::search::QueryIncompleteReason::Unavailable)
        } else {
            None
        }
    }

    fn refresh_search_index(&self, _ctx: &SearchContext<'_>) {
        self.refresh_snapshot();
    }

    /// An address is looked up in the whole snapshot. An empty query's first
    /// page holds [`SearchBudget::PALETTE`] rows, fewer than a desktop has
    /// host apps, so resolving through it left every later app unreachable
    /// by id.
    fn resolve(&self, id: &str, ctx: SearchContext<'_>) -> Option<SearchResult> {
        if !ctx.wanted.contains(&ResourceKind::HostApplication) {
            return None;
        }
        if self.snapshot().is_none() {
            self.settle_snapshot(crate::shell::search::RESOLVE_INDEX_WAIT);
        }
        let snapshot = self.snapshot()?;
        snapshot
            .apps
            .iter()
            .find(|app| {
                app.desktop_id == id
                    && !snapshot
                        .installed_ids
                        .contains(&app.desktop_id.to_ascii_lowercase())
            })
            .map(|app| self.row(app, score_for(ctx.needle, None)))
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
        let offset = match cursor {
            Some(cursor) if cursor.revision != revision => return SearchPage::stale(revision),
            Some(cursor) => match cursor.token.strip_prefix("host:").and_then(|token| {
                token
                    .split_once('\u{1f}')
                    .filter(|(_, query)| *query == ctx.needle)
                    .and_then(|(offset, _)| offset.parse::<usize>().ok())
            }) {
                Some(offset) => offset,
                None => return SearchPage::stale(revision),
            },
            None => 0,
        };
        let found = snapshot.index.search(
            ctx.needle,
            Page {
                offset,
                limit: budget.max_work.min(budget.max_results),
            },
        );
        let mut shown = 0usize;
        let mut bytes = 0usize;
        let mut results = Vec::new();
        for hit in &found.hits {
            let Some(app) = usize::try_from(hit.key)
                .ok()
                .and_then(|at| snapshot.apps.get(at))
            else {
                continue;
            };
            let row = self.row(app, score_for(ctx.needle, hit.rank));
            let row_bytes = row
                .title
                .len()
                .saturating_add(row.subtitle.as_ref().map_or(0, String::len));
            if bytes.saturating_add(row_bytes) > budget.max_bytes {
                break;
            }
            bytes = bytes.saturating_add(row_bytes);
            shown = shown.saturating_add(1);
            results.push(row);
        }
        let exhausted = !found.more && shown == found.hits.len();
        let next = offset.saturating_add(shown);
        SearchPage {
            revision: revision.clone(),
            results,
            next_cursor: (!exhausted).then(|| SearchCursor {
                revision,
                token: format!("host:{next}\u{1f}{}", ctx.needle),
            }),
            exhausted,
            stale_cursor: false,
            budget_exhausted: !exhausted,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::shell::chrome_state::ChromeState;
    use crate::shell::state::{WorkspaceTree, WorkspaceTreeHandle};
    use automerge::AutoCommit;
    use std::sync::Arc;

    fn chrome_rc() -> Chrome {
        let handle: WorkspaceTreeHandle =
            crate::shell::state::WorkspaceTreeHandle::new(WorkspaceTree::default());
        ChromeState::new_with_handle(handle)
    }

    fn app(desktop_id: &str, name: &str) -> HostApp {
        HostApp {
            desktop_id: desktop_id.to_string(),
            name: name.to_string(),
            source_label: "Flatpak",
            categories: Vec::new(),
            keywords: Vec::new(),
            mime_types: Vec::new(),
            terminal: false,
            icon: None,
            window_identities: vec![desktop_id.to_string()],
        }
    }

    #[test]
    fn catalogue_completion_waits_for_actual_worker() {
        use crate::shell::chrome_state::SearchScope;
        use crate::shell::search::action_api::{ActionCatalog, AllVisible};
        use crate::shell::search::{QueryArgs, QueryIncompleteReason, SearchRegistry};
        let source = HostAppsTestSource::new(Vec::new(), Vec::new());
        let gate = source.state.lock().expect("hold source materialization");
        let mut registry = SearchRegistry::new();
        registry.register(std::rc::Rc::new(
            HostApplicationsProvider::with_test_source(source.clone()),
        ));
        let chrome = chrome_rc();
        let doc = AutoCommit::new();
        let args = QueryArgs {
            needle: "",
            scope: SearchScope::All,
            active_provider_id: None,
            wanted_kinds: &[],
            chrome: &chrome.borrow(),
            doc: &doc,
            doc_root: &automerge::ROOT,
        };
        let pending = ActionCatalog::build_query(&registry, &args, &AllVisible, true);
        assert!(pending.enumerate().is_empty());
        assert!(!pending.completion().complete);
        assert_eq!(
            pending
                .completion()
                .incomplete
                .first()
                .expect("pending source")
                .reason,
            QueryIncompleteReason::Pending
        );
        drop(gate);
        let deadline = tilo_test_env::hang_deadline();
        loop {
            let ready = ActionCatalog::build_query(&registry, &args, &AllVisible, true);
            assert!(ready.enumerate().is_empty());
            if ready.completion().complete {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "actual empty index did not become ready"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    #[test]
    fn host_app_matching_lists_offers_or_returns_nothing() {
        // Phase `a_matching_host_app_offers_an_open_row`.
        {
            let p = HostApplicationsProvider::new();
            let out = p.results_against("spot", &[app("com.spotify.Client", "Spotify")], &[]);
            assert_eq!(out.len(), 1);
            assert_eq!(out[0].title, "Open Spotify");
            assert_eq!(out[0].id, "com.spotify.Client");
            assert_eq!(out[0].kind, ResourceKind::HostApplication);
        }

        // Phase `keywords_and_categories_are_matchable`.
        {
            let p = HostApplicationsProvider::new();
            let mut firefox = app("org.mozilla.firefox", "Firefox");
            firefox.keywords = vec!["Internet".into(), "WWW".into()];
            firefox.categories = vec!["Network".into(), "WebBrowser".into()];
            let by_keyword = p.results_against("internet", &[firefox.clone()], &[]);
            assert_eq!(by_keyword.len(), 1, "{by_keyword:?}");
            let by_category = p.results_against("webbrowser", &[firefox], &[]);
            assert_eq!(by_category.len(), 1, "{by_category:?}");
            assert!(
                by_keyword[0].score > by_category[0].score,
                "a declared keyword is a stronger signal than a category bucket"
            );
        }

        // Phase `an_empty_needle_lists_every_host_app`.
        {
            let p = HostApplicationsProvider::new();
            let out = p.results_against("", &[app("a.b.One", "One"), app("a.b.Two", "Two")], &[]);
            assert_eq!(out.len(), 2);
            assert!(out.iter().all(|r| r.score == 0));
        }

        // Phase `a_non_matching_needle_returns_nothing`.
        {
            let p = HostApplicationsProvider::new();
            assert!(
                p.results_against("zzzqqq", &[app("com.spotify.Client", "Spotify")], &[])
                    .is_empty()
            );
        }

        // Phase `unrequested_kind_returns_empty`.
        {
            let p = HostApplicationsProvider::new();
            let c = chrome_rc();
            let cs = c.borrow();
            let doc = AutoCommit::new();
            let out = p.query(SearchContext {
                query_scope: None,
                action_scope: None,
                include_programmatic: false,
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
    }

    #[test]
    fn action_catalog_discovers_cold_host_apps_without_opening_palette() {
        use crate::shell::chrome_state::SearchScope;
        use crate::shell::search::SearchRegistry;
        use crate::shell::search::action_api::{ActionCatalog, AllVisible, ResourceRef};
        use std::rc::Rc;
        use std::time::{Duration, Instant};

        let source = HostAppsTestSource::new(
            vec![app("org.fixture.Calculator", "Calculator")],
            Vec::new(),
        );
        let mut registry = SearchRegistry::new();
        registry.register(Rc::new(HostApplicationsProvider::with_test_source(source)));
        let chrome = chrome_rc();
        let doc = AutoCommit::new();
        let target = ResourceRef::new("host-applications", "org.fixture.Calculator");
        let deadline = tilo_test_env::hang_deadline();
        loop {
            let catalog = ActionCatalog::build(
                &registry,
                "Calculator",
                SearchScope::All,
                None,
                &chrome.borrow(),
                &doc,
                &automerge::ROOT,
                &AllVisible,
            );
            if let Some(action) = catalog.select(&target) {
                assert_eq!(action.title(), "Open Calculator");
                break;
            }
            assert!(
                Instant::now() < deadline,
                "remote action discovery must populate a cold host-app index without palette input"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn every_host_app_is_addressable_past_the_first_palette_page() {
        use crate::shell::chrome_state::SearchScope;
        use crate::shell::search::SearchRegistry;
        use crate::shell::search::action_api::{ActionCatalog, AllVisible, ResourceRef};
        use std::rc::Rc;

        let count = SearchBudget::PALETTE.max_results + 12;
        let mut apps: Vec<HostApp> = (0..count)
            .map(|index| {
                app(
                    &format!("org.fixture.App{index:03}"),
                    &format!("App {index:03}"),
                )
            })
            .collect();
        apps.push(app("so.tilo.terminal", "Terminal"));
        let source = HostAppsTestSource::new(apps, vec![installed("so.tilo.terminal", "Terminal")]);
        let provider = Rc::new(HostApplicationsProvider::with_test_source(source));
        let chrome = chrome_rc();
        let doc = AutoCommit::new();
        {
            let chrome = chrome.borrow();
            let ctx = SearchContext {
                query_scope: None,
                action_scope: None,
                include_programmatic: false,
                needle: "",
                wanted: &[ResourceKind::HostApplication],
                chrome: &chrome,
                doc: &doc,
                doc_root: &automerge::ROOT,
            };
            let deadline = tilo_test_env::hang_deadline();
            while !provider.search_revision(&ctx).starts_with("ready:") {
                assert!(
                    std::time::Instant::now() < deadline,
                    "host-app index never became ready"
                );
                provider.refresh_search_index(&ctx);
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            assert!(
                provider
                    .query_page(ctx, None, SearchBudget::PALETTE)
                    .results
                    .len()
                    < count,
                "the fixture must not fit on the first page"
            );
        }
        let mut registry = SearchRegistry::new();
        registry.register(provider);
        let select = |id: &str| {
            let resource = ResourceRef::new("host-applications", id);
            ActionCatalog::build_for_resource(
                &registry,
                &resource,
                SearchScope::All,
                None,
                &chrome.borrow(),
                &doc,
                &automerge::ROOT,
                &AllVisible,
            )
            .select(&resource)
            .map(|action| action.title().to_string())
        };
        let last = format!("org.fixture.App{:03}", count - 1);
        assert_eq!(
            select(&last).as_deref(),
            Some(format!("Open App {:03}", count - 1).as_str()),
            "an app past the first page must still open by its address"
        );
        assert_eq!(
            select("so.tilo.terminal"),
            None,
            "a host app an installed Tilo package answers for stays hidden"
        );
        assert_eq!(select("org.fixture.Missing"), None);
    }

    #[test]
    fn cold_and_warm_public_pages_never_rescan_a_hundred_thousand_host_apps() {
        let chrome = chrome_rc();
        let chrome = chrome.borrow();
        let doc = AutoCommit::new();
        let ctx = SearchContext {
            query_scope: None,
            action_scope: None,
            include_programmatic: false,
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
        let apps = (0..=4_000)
            .map(|index| {
                app(
                    &format!("org.fixture.App{index}"),
                    if index == 4_000 {
                        "unique-tail"
                    } else {
                        "common app"
                    },
                )
            })
            .collect::<Vec<_>>();
        let index = index_apps(&apps, &HashSet::new());
        *provider.snapshot.borrow_mut() = Some(HostAppsSnapshot {
            source_generation: provider.source_generation(),
            apps,
            installed_ids: HashSet::new(),
            index,
        });
        let page = provider.query_page(
            SearchContext {
                query_scope: None,
                action_scope: None,
                include_programmatic: false,
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
            (0..=4_000)
                .map(|index| {
                    app(
                        &format!("org.fixture.Source{index}"),
                        if index == 4_000 {
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
            query_scope: None,
            action_scope: None,
            include_programmatic: false,
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

    #[test]
    fn host_app_rows_are_a_separate_lower_ranked_kind_that_names_its_provenance() {
        // Phase `the_row_names_the_source_and_says_it_is_unconfined`.
        // "Unconfined" in the subtitle is the whole point of the separate kind.
        {
            let p = HostApplicationsProvider::new();
            let out = p.results_against("spot", &[app("com.spotify.Client", "Spotify")], &[]);
            let subtitle = out[0].subtitle.as_deref().expect("a subtitle");
            assert!(subtitle.contains("Flatpak"), "{subtitle}");
            assert!(
                subtitle.contains("unconfined"),
                "the user must be able to tell a confined Tilo app from an \
                 unconfined host app without opening anything: {subtitle}"
            );
            assert!(subtitle.contains("com.spotify.Client"), "{subtitle}");
        }

        // Phase `a_system_app_row_names_system_provenance`.
        {
            let p = HostApplicationsProvider::new();
            let system = HostApp {
                source_label: "System",
                ..app("org.gnome.Console", "Terminal")
            };
            let out = p.results_against("terminal", &[system], &[]);
            let subtitle = out[0].subtitle.as_deref().expect("a subtitle");
            assert!(subtitle.starts_with("System · unconfined host app ·"));
        }

        // Phase `host_apps_are_a_separate_kind_from_tilo_applications`.
        {
            let p = HostApplicationsProvider::new();
            assert_eq!(p.resource_kinds(), &[ResourceKind::HostApplication]);
            assert_eq!(ResourceKind::HostApplication.label(), "Host Apps");
            assert_ne!(
                ResourceKind::HostApplication.label(),
                ResourceKind::Application.label()
            );
        }

        // Phase `a_host_app_scores_below_an_equally_good_tilo_match`.
        // Equal-quality matches rank the confined app first — Enter takes the top row.
        {
            use crate::shell::search::providers::applications::ApplicationsProvider;
            let host = HostApplicationsProvider::new();
            let tilo = ApplicationsProvider::new();
            // Different name/id tail, so this measures the scale, not shadowing.
            let host_rows =
                host.results_against("term", &[app("org.x.Terminator", "Terminator")], &[]);
            let tilo_rows = tilo.results_for(
                "term",
                vec![crate::shell::launcher::InstalledApp {
                    package_id: "so.tilo.terminal".into(),
                    display_name: "Terminal".into(),
                    localized_names: BTreeMap::default(),
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
    }

    /// Must surface a visible error, not fail silently. Drives the
    /// `on_open` closure.
    #[test]
    fn a_failed_launch_pushes_an_error_notification() {
        let p = HostApplicationsProvider::new();
        let c = chrome_rc();
        let mut doc = AutoCommit::new();
        let out = p.results_against("ghost", &[app("so.tilo.ghost-host-app", "Ghost")], &[]);
        let row = out.first().expect("a row for the ghost app");
        assert!(c.borrow().notifications().is_empty());
        (row.on_open)(
            &mut ui::widget::Edits::lend(&mut doc),
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
            localized_names: BTreeMap::default(),
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
    fn only_an_exact_tilo_package_identity_hides_a_host_row() {
        // Phase `only_an_exact_tilo_package_identity_hides_the_host_row`.
        {
            let installed = vec![installed("so.tilo.terminal", "Terminal")];
            assert!(shadowed_by_a_tilo_package(
                &app("so.tilo.terminal", "Terminal"),
                &installed
            ));
        }

        // Phase `tilo_terminal_does_not_hide_a_distinct_host_terminal`.
        {
            let installed = vec![installed("so.tilo.terminal", "Terminal")];
            assert!(!shadowed_by_a_tilo_package(
                &app("org.gnome.Console", "Terminal"),
                &installed
            ));
        }

        // Phase `tilo_calendar_does_not_hide_gnome_calendar`.
        {
            let installed = vec![installed("so.tilo.calendar", "Calendar")];
            assert!(!shadowed_by_a_tilo_package(
                &app("org.gnome.Calendar", "Calendar"),
                &installed
            ));
        }

        // Phase `a_generic_shared_name_does_not_hide_a_host_app`.
        {
            let installed = vec![installed("so.tilo.notes", "Notes")];
            assert!(!shadowed_by_a_tilo_package(
                &app("org.example.PrivateNotes", "Notes"),
                &installed
            ));
        }
    }
}
