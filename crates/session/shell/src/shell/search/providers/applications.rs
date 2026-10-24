//! Applications provider: apps you can open, launchable from the palette.
//!
//! Typing an installed app's name into Ctrl+K offers `Open <App>`, launched
//! through [`crate::shell::launcher`]. Apps the device does not have yet are
//! the catalogue's, which search shares with the App Store
//! ([`super::store_catalogue`]).

use crate::shell::state::WorkspaceAuthority as _;
use std::cell::Ref;
use std::collections::HashSet;
use std::sync::Arc;
#[cfg(test)]
use std::sync::Mutex;

use super::background_index::BackgroundIndex;
use crate::portal::desktop_services::DynamicLauncherRecord;
use crate::shell::catalogue;
use crate::shell::chrome_state::{Chrome, OpenTarget};
use crate::shell::i18n::{t, t_args};
use crate::shell::launcher::{self, InstalledApp};
#[cfg(test)]
use crate::shell::search::action_api::ActionArgs;
use crate::shell::search::action_api::ActionError;
use crate::shell::search::provider_score;
use crate::shell::search::{
    ResourceKind, SearchBudget, SearchContext, SearchCursor, SearchPage, SearchProvider,
    SearchResult,
};
use tilo_desktop::notifications::{Notification, NotificationSeverity};
use tilo_search::{Document, Matching, SearchIndex};

/// Chip label / group header for launchable apps; also the provider's `name()`.
pub const APPLICATIONS_KIND: &str = "Applications";

/// The provider's own id, which also keys what was chosen from it.
const PROVIDER_ID: &str = "applications";

/// `open-with:<package id>` addresses an installed app that opens the files
/// and links an invocation names (`{"open": ["/home/me/a.md"]}`), through the
/// same "Open with" path Files uses. It is resolved by address only and never
/// listed: the palette's row for the app stays the nullary one, so Enter,
/// Shift+Enter and Ctrl+Enter keep opening the app itself.
pub(crate) const OPEN_WITH_PREFIX: &str = "open-with:";

/// Most files and links one invocation opens.
const OPEN_WITH_MAX_ITEMS: usize = 64;

/// The `open-with:` row for `app`: its one parameter is the list to open.
fn open_with_row(app: Arc<InstalledApp>) -> SearchResult {
    let package_id = app.package_id.clone();
    let mut row = SearchResult::nullary_fallible(
        ResourceKind::Application,
        PROVIDER_ID,
        format!("{OPEN_WITH_PREFIX}{package_id}"),
        app.name_for(&crate::shell::i18n::current_locale()),
        app_detail(&app.package_id, app.category.as_deref()),
        provider_score(0),
        |_doc, _root, _chrome: &Chrome| Ok(()),
    );
    row.params = vec![
        crate::shell::search::action_api::ActionParam::required(
            "open",
            crate::shell::search::action_api::ParamType::Array,
            "Absolute paths of files, or links, for the app to open",
        )
        .with_schema(serde_json::json!({
            "minItems": 1,
            "maxItems": OPEN_WITH_MAX_ITEMS,
            "items": {"type": "string", "minLength": 1},
        })),
    ];
    row.on_open = std::rc::Rc::new(move |_doc, _root, chrome: &Chrome, args| {
        open_items_with(chrome, &app, args)
    });
    row
}

/// Open each of `args`' `open` items with exactly `app`: a file through the
/// "Open with" path (the app must claim its type), a link when `app` is what
/// opens links of its kind. Stops at the first that does not open.
fn open_items_with(
    chrome: &Chrome,
    app: &InstalledApp,
    args: &crate::shell::search::action_api::ActionArgs,
) -> Result<(), ActionError> {
    let name = app
        .name_for(&crate::shell::i18n::current_locale())
        .to_owned();
    let items = args.array("open").unwrap_or_default();
    for item in items {
        let Some(item) = item.as_str() else {
            continue;
        };
        let path = std::path::Path::new(item);
        if path.is_absolute() {
            crate::shell::doc_open::open_with_app(path, chrome, &app.package_id).map_err(
                |error| {
                    ActionError::Explained(t_args(
                        "search-open-with-file-failed",
                        &[
                            ("app", name.clone().into()),
                            ("file", item.to_owned().into()),
                            ("reason", error.user_message().into()),
                        ],
                    ))
                },
            )?;
            continue;
        }
        let handler = {
            let state = chrome.borrow();
            let tree = state.tree();
            crate::shell::uri_open::resolve_for(
                item,
                &state.local_user_id,
                tree.desktop
                    .active_workspace()
                    .map(|workspace| &workspace.default_apps),
            )
        };
        match handler {
            Ok(handler) if handler.package_id == app.package_id => {
                crate::shell::uri_open::open_in_workspace(item, chrome)
                    .map(drop)
                    .map_err(|error| ActionError::Explained(error.user_message()))?;
            }
            Ok(_) | Err(crate::shell::uri_open::UriOpenError::NoHandler { .. }) => {
                return Err(ActionError::Explained(t_args(
                    "search-open-with-link-other-app",
                    &[("app", name.into()), ("link", item.to_owned().into())],
                )));
            }
            Err(error) => return Err(ActionError::Explained(error.user_message())),
        }
    }
    Ok(())
}

fn presentation_score(needle: &str, package_id: &str, match_score: i32) -> i32 {
    if needle.is_empty() {
        // The apps used most lead the empty list within the order the
        // product gives it.
        ui::search::system::starting_point_rank(package_id)
            .saturating_add(crate::shell::search::usage::boost(PROVIDER_ID, package_id))
            .min(provider_score(100))
    } else {
        match_score
    }
}

/// An app row's second line: the tag line the Tilo fleet publishes for it,
/// else its category, else nothing. The package id, vendor and version are
/// how a developer tells builds apart, not how a person recognises an app;
/// the id stays searchable, never shown.
fn app_detail(package_id: &str, category: Option<&str>) -> Option<String> {
    app_catalogue::fleet::metadata(package_id)
        .map(|app| app.tagline.trim())
        .filter(|tagline| !tagline.is_empty())
        .or_else(|| {
            category
                .map(str::trim)
                .filter(|category| !category.is_empty())
        })
        .map(str::to_owned)
}

/// The installed apps', the dynamic launchers' and the desktop entries'
/// generations: the last carries the apps the person took off their apps.
type Generation = (u64, u128, u64);

pub struct ApplicationsProvider {
    kinds: Vec<ResourceKind>,
    index: BackgroundIndex<Generation, ApplicationsSnapshot>,
    #[cfg(test)]
    source: Option<Arc<ApplicationsTestSource>>,
}

#[cfg(test)]
type ApplicationsTestState = (Vec<InstalledApp>, Vec<DynamicLauncherRecord>);

#[cfg(test)]
pub(crate) struct ApplicationsTestSource {
    generation: std::sync::atomic::AtomicU64,
    state: Mutex<ApplicationsTestState>,
    materializations: std::sync::atomic::AtomicUsize,
}

#[cfg(test)]
impl ApplicationsTestSource {
    pub(crate) fn new(
        installed: Vec<InstalledApp>,
        dynamic: Vec<DynamicLauncherRecord>,
    ) -> Arc<Self> {
        Arc::new(Self {
            generation: std::sync::atomic::AtomicU64::new(1),
            state: Mutex::new((installed, dynamic)),
            materializations: std::sync::atomic::AtomicUsize::new(0),
        })
    }

    fn generation(&self) -> Generation {
        let generation = self.generation.load(std::sync::atomic::Ordering::Acquire);
        (generation, u128::from(generation), 0)
    }

    fn snapshot(&self) -> (u64, Vec<InstalledApp>, Vec<DynamicLauncherRecord>) {
        self.materializations
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let generation = self.generation.load(std::sync::atomic::Ordering::Acquire);
        let (installed, dynamic) = self
            .state
            .lock()
            .expect("application test source lock")
            .clone();
        (generation, installed, dynamic)
    }

    fn replace_installed(&self, installed: Vec<InstalledApp>) {
        self.state.lock().expect("application test source lock").0 = installed;
        self.generation
            .fetch_add(1, std::sync::atomic::Ordering::Release);
    }
}

enum ApplicationEntry {
    /// Shared with the launch callback of every row built for it, so a
    /// keystroke does not copy the app's manifest view into each row.
    Installed(Arc<InstalledApp>),
    Dynamic(DynamicLauncherRecord),
}

struct ApplicationsSnapshot {
    entries: Vec<ApplicationEntry>,
    /// Canonical lower-cased identities make the warm membership check of a
    /// shortcut's owner O(1), rather than a walk of every installed app per
    /// indexed row.
    installed_ids: HashSet<String>,
    /// Entries' fields, keyed by position in `entries`.
    index: SearchIndex,
}

impl ApplicationsProvider {
    pub fn new() -> Self {
        Self {
            kinds: vec![ResourceKind::Application],
            index: BackgroundIndex::new("tilo-app-search-index"),
            #[cfg(test)]
            source: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_test_source(source: Arc<ApplicationsTestSource>) -> Self {
        Self {
            source: Some(source),
            ..Self::new()
        }
    }

    /// How many times this provider has built its index.
    #[cfg(test)]
    pub(crate) fn index_builds(&self) -> usize {
        self.index
            .probe
            .builds
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    #[allow(clippy::unused_self, reason = "test builds use the injected source")]
    fn source_generation(&self) -> Generation {
        #[cfg(test)]
        if let Some(source) = &self.source {
            return source.generation();
        }
        (
            launcher::gui_apps_generation(),
            crate::portal::desktop_services::dynamic_launcher_generation(),
            desktop_entry::cache::cached_scan_generation(),
        )
    }

    /// Refresh the retained projection during the registry's source-refresh
    /// pass.  `query_page` deliberately never calls this: a source change may
    /// contain a very large installed corpus and must not turn the first
    /// keystroke into an indexing job.
    fn build_snapshot(
        #[cfg(test)] source: Option<Arc<ApplicationsTestSource>>,
    ) -> (Generation, ApplicationsSnapshot) {
        #[cfg(test)]
        if let Some(source) = source {
            let (generation, installed, dynamic) = source.snapshot();
            return (
                (generation, u128::from(generation), 0),
                Self::snapshot_from_parts(&installed, dynamic, &[]),
            );
        }
        let (installed_generation, installed) = launcher::gui_apps_snapshot();
        let (dynamic_generation, dynamic) =
            crate::portal::desktop_services::dynamic_launcher_snapshot();
        let (entries_generation, scan) = desktop_entry::cache::cached_scan_snapshot();
        (
            (installed_generation, dynamic_generation, entries_generation),
            Self::snapshot_from_parts(&installed, dynamic, &scan.hidden),
        )
    }

    /// The index over `installed` and `dynamic`, less the installed apps
    /// whose ids are in `removed_from_apps`.
    fn snapshot_from_parts(
        installed: &[InstalledApp],
        dynamic: Vec<DynamicLauncherRecord>,
        removed_from_apps: &[String],
    ) -> ApplicationsSnapshot {
        let installed_ids: HashSet<String> = installed
            .iter()
            .map(|app| app.package_id.to_ascii_lowercase())
            .collect();
        let mut entries = installed
            .iter()
            .filter(|app| !removed_from_apps.contains(&app.package_id))
            .cloned()
            .map(|app| ApplicationEntry::Installed(Arc::new(app)))
            .collect::<Vec<_>>();
        entries.extend(dynamic.into_iter().map(ApplicationEntry::Dynamic));
        let index = index_entries(&entries);
        ApplicationsSnapshot {
            entries,
            installed_ids,
            index,
        }
    }

    /// The worker reads and clones every source itself; the UI tick never
    /// flattens an application corpus before spawn, and a query never starts it.
    #[allow(clippy::unused_self, reason = "test builds use the injected source")]
    fn builder(&self) -> impl FnOnce() -> (Generation, ApplicationsSnapshot) + Send + 'static {
        #[cfg(test)]
        let source = self.source.clone();
        move || {
            Self::build_snapshot(
                #[cfg(test)]
                source,
            )
        }
    }

    fn snapshot(&self) -> Option<Ref<'_, ApplicationsSnapshot>> {
        self.index.current(&self.source_generation())
    }

    /// Score `apps` against `needle`, as [`Self::installed_rows`].
    #[cfg(test)]
    pub(crate) fn results_for(&self, needle: &str, apps: Vec<InstalledApp>) -> Vec<SearchResult> {
        let scores = test_scores(needle, apps.iter().map(installed_fields));
        self.installed_rows(needle, apps.into_iter().map(Arc::new).collect(), &scores)
    }

    /// Launch rows for the `apps` with a score in `scores`. Where the
    /// window will land is not on the row: it is the same every time (beside
    /// the window you're in), so saying it on every result only costs a line.
    /// The launch reads the live target at activation time.
    fn installed_rows(
        &self,
        needle: &str,
        apps: Vec<Arc<InstalledApp>>,
        scores: &[Option<i32>],
    ) -> Vec<SearchResult> {
        let kind = ResourceKind::Application;
        let mut out = Vec::new();
        for (app, score) in apps.into_iter().zip(scores) {
            let Some(score) = *score else {
                continue;
            };
            let launch_app = Arc::clone(&app);
            let detail = app_detail(&app.package_id, app.category.as_deref());
            out.push(SearchResult::nullary_fallible(
                kind.clone(),
                self.id(),
                app.package_id.clone(),
                app.name_for(&crate::shell::i18n::current_locale()),
                detail,
                presentation_score(needle, &app.package_id, score),
                move |_doc, _root, chrome: &Chrome| {
                    launch_resolved_installed_app(chrome, &launch_app)
                },
            ));
        }
        out
    }

    /// Reviewed `DynamicLauncher` entries are catalogue aliases, never command
    /// lines. Application entries start only their authenticated owner package;
    /// web entries pass their HTTPS target through the normal URI handler and
    /// workspace policy.
    #[cfg(test)]
    fn dynamic_results_for(
        &self,
        needle: &str,
        records: Vec<DynamicLauncherRecord>,
        installed: &[InstalledApp],
    ) -> Vec<SearchResult> {
        let installed_gui = installed
            .iter()
            .map(|app| app.package_id.to_ascii_lowercase())
            .collect();
        let scores = test_scores(
            needle,
            records
                .iter()
                .map(|record| searchable_fields(&record.name, &record.id, None, None, [], &[])),
        );
        self.dynamic_results_for_membership(records, &scores, &installed_gui)
    }

    fn dynamic_results_for_membership(
        &self,
        records: Vec<DynamicLauncherRecord>,
        scores: &[Option<i32>],
        installed_gui: &HashSet<String>,
    ) -> Vec<SearchResult> {
        records
            .into_iter()
            .zip(scores)
            .filter_map(|(record, score)| {
                let score = (*score)?;
                let valid = match record.launcher_type {
                    1 => {
                        record.target.is_none()
                            && installed_gui.contains(&record.owner_app_id.to_ascii_lowercase())
                    }
                    2 => record.target.as_deref().is_some_and(|target| {
                        target.starts_with("https://")
                            && crate::shell::uri_open::resource_for_uri(target).is_ok()
                    }),
                    _ => false,
                };
                if !valid
                    || record.name.trim().is_empty()
                    || record.name.chars().any(char::is_control)
                {
                    return None;
                }
                let name = record.name.clone();
                let owner = record.owner_app_id.clone();
                let target = record.target.clone();
                let launcher_type = record.launcher_type;
                Some(SearchResult::nullary_fallible(
                    ResourceKind::Application,
                    self.id(),
                    record.id,
                    record.name,
                    Some(t("search-apps-shortcut-subtitle")),
                    score,
                    move |_doc, _root, chrome: &Chrome| {
                        if launcher_type == 1 {
                            return launch_installed_app(chrome, &owner, &name);
                        }
                        let Some(target) = target.as_deref() else {
                            return Err(ActionError::Refused("Shortcut target is missing".into()));
                        };
                        crate::shell::uri_open::open_in_workspace(target, chrome)
                            .map(|_| ())
                            .map_err(|error| {
                                let message = format!("Couldn't open {name}: {error:?}");
                                let mut state = chrome.borrow_mut();
                                state.push_notification_full(
                                    Notification::new(
                                        t_args(
                                            "launch-failure-title",
                                            &[("app", name.as_str().into())],
                                        ),
                                        // The URI resolver's Debug output may
                                        // contain package ids, paths, or
                                        // multiline backend details. Keep the
                                        // full value in the refusal returned
                                        // to the caller/log and give the
                                        // fixed-height toast a bounded recovery
                                        // instruction instead.
                                        t("search-apps-shortcut-failed"),
                                    )
                                    .with_severity(NotificationSeverity::Error)
                                    .with_app_name(t("host-source-applications")),
                                );
                                state.bump();
                                ActionError::Refused(message)
                            })
                    },
                ))
            })
            .collect()
    }
}

/// The package id of the file manager Tilo ships as a system app.
pub(crate) const SYSTEM_FILE_MANAGER: &str = "so.tilo.file-explorer";

/// Give the system file manager the user's home before it starts. Every app
/// otherwise gets `HOME` pointed at its own directory, wrong for one whose
/// subject is the user's files. Not a hole in the model: the grant goes
/// through the same [`GrantStoreHandle`](crate::portal::GrantStoreHandle) the
/// permission prompt writes to — visible and revocable, scoped to the
/// session, just decided by the OS instead of a dialog. Names one package id
/// rather than a category rule a third party could satisfy by declaring the
/// right thing about itself.
fn grant_user_home_to_file_manager(chrome: &Chrome, package_id: &str) {
    let ws_id = {
        let st = chrome.borrow();
        if st.tree().desktop.active_workspace().is_none() {
            return;
        }
        st.tree().desktop.active
    };
    let granted = {
        let st = chrome.borrow_mut();
        let mut tree = st.tree_mut();
        grant_file_manager_home(&mut tree, ws_id, package_id)
    };
    if granted {
        chrome.borrow_mut().bump();
    }
}

/// [`grant_user_home_to_file_manager`] against a tree, for launches that
/// don't come from the palette (also reached from
/// [`crate::shell::peer_windows`]). Returns whether a grant was added, so a
/// caller with a repaint to do can skip it when nothing changed.
///
/// Every member mints their own rather than receiving a copy, since the path
/// names *this* machine's home and is meaningless on anybody else's. Doesn't
/// mirror into the room document the way `GrantStoreHandle::insert` does,
/// but the workspace projection writes `ws.sandbox` wholesale
/// (`state.rs`), so the grant still reaches every peer on the next persist.
pub(crate) fn grant_file_manager_home(
    tree: &mut crate::shell::state::WorkspaceTree,
    workspace: crate::shell::state::WorkspaceId,
    package_id: &str,
) -> bool {
    if package_id != SYSTEM_FILE_MANAGER {
        return false;
    }
    let Some(home) = tilo_paths::home() else {
        return false;
    };
    grant_file_manager_path(tree, workspace, home)
}

/// Give the system file manager `path`, read-write, for this session, the
/// way [`grant_file_manager_home`] gives it the home: through the workspace's
/// grants, visible and revocable. Also used for a drive the person just
/// opened ([`crate::shell::drives`]). Returns whether a grant was added.
pub(crate) fn grant_file_manager_path(
    tree: &mut crate::shell::state::WorkspaceTree,
    workspace: crate::shell::state::WorkspaceId,
    path: std::path::PathBuf,
) -> bool {
    use crate::shell::state::{Access, Capability, FsMode, GRANT_SESSION, ResourceRef};

    let package_id = SYSTEM_FILE_MANAGER;
    let Some(ws) = tree
        .desktop
        .workspaces
        .iter_mut()
        .find(|w| w.id == workspace)
    else {
        return false;
    };
    // Asks the capability model rather than re-deriving prefix logic.
    let actor = ws.actor_for_app(package_id);
    if ws.allows(
        &actor,
        &ResourceRef::FsPath(path.to_string_lossy().into_owned()),
        Access::Read,
    ) {
        return false;
    }
    let ids = ws.grant_app(
        package_id,
        &Capability::Filesystem {
            path,
            mode: FsMode::ReadWrite,
            scope: GRANT_SESSION,
        },
    );
    let remembered = tree.native_runtime.remember_files_home_consent(ws, &ids);
    if !remembered && tree.native_authority.requires(ws.room()) {
        // Native session consent must have exact live local provenance. A
        // missing identity or full ledger must not leave an unbound row.
        ws.sandbox
            .retain(|capability| !ids.contains(&capability.id));
        return false;
    }
    // The launch that follows captures this grant, and its sandbox is
    // released only once the accepted policy holds it: publish it with a
    // save, which the launch waits for. Nothing else may save before then:
    // when Files is already an author (any session after the first, the
    // home grant ending with each session while authorship persists) every
    // later open of it was refused for the rest of the session.
    tree.queue_save();
    true
}

/// Launch an installed package into the active workspace, reporting a
/// pre-fork failure as a notification. Shared by the Applications row and
/// the catalog's post-install launch, so "the app opens" means the same
/// placement, grants, and room everywhere; the workspace id becomes the
/// collaboration room it joins. Grants are read fresh, never cached, so a
/// permission revoked a moment ago is honoured here.
pub(crate) fn launch_installed_app(
    chrome: &Chrome,
    package_id: &str,
    display_name: &str,
) -> Result<(), ActionError> {
    // An activation already accepted for this package owns the single launch
    // worker. Check that transaction before consulting the
    // registry: the package can disappear while its queued worker is still
    // resolving, but a repeated click must not turn that existing launch into
    // an unrelated "not installed" error.
    if catalogue::pending_launch(package_id) {
        return Ok(());
    }
    let app = launcher::installed_apps().and_then(|apps| {
        apps.into_iter()
            .find(|app| app.package_id == package_id)
            .ok_or_else(|| launcher::LaunchError::NotInstalled {
                package_id: package_id.to_string(),
            })
    });
    match app {
        Ok(app) => launch_resolved_installed_app(chrome, &app),
        Err(err) => Err(report_refused_launch(
            chrome,
            package_id,
            display_name,
            &err,
        )),
    }
}

/// The device's control panel: one window per machine, so opening it again,
/// from Quick Settings, the palette or anywhere else, focuses that window.
const SETTINGS_PACKAGE_ID: &str = "so.tilo.settings";

/// Surface a launch that failed before spawn: settle its pending launch, post
/// the failure notification, and describe it for callers that cannot see
/// the screen.
fn report_refused_launch(
    chrome: &Chrome,
    package_id: &str,
    display_name: &str,
    err: &launcher::LaunchError,
) -> ActionError {
    // The preflight failed before a toplevel could claim the placement
    // note. Do not let a later app inherit this dead launch's target.
    launcher::clear_pending_placement(package_id);
    let message = err.user_message();
    tracing::error!(
        target: "tilo::app_launch",
        package_id,
        display_name,
        detail = %message,
        "application launch was refused before spawn"
    );
    {
        let mut st = chrome.borrow_mut();
        // Settled now, or the app could never be opened again this session.
        catalogue::take_pending_launch(package_id);
        st.push_notification_full(
            Notification::new(
                launcher::launch_failure_title(display_name),
                err.notification_message(),
            )
            .with_severity(NotificationSeverity::Error)
            .with_app_name(t("host-source-applications"))
            .with_dismiss_after(launcher::LAUNCH_FAILURE_NOTICE_LIFETIME),
        );
        st.bump();
    }
    ActionError::Refused(format!("Couldn't open {display_name}: {message}"))
}

/// Reopening the app while its saved card is focused recovers that window
/// and acknowledges queued/opening recovery without another document. Other activation keeps its
/// ordinary placement; no failed sibling is selected by app identity alone.
fn reopen_focused_local_card(chrome: &Chrome, package_id: &str) -> bool {
    let target = {
        let state = chrome.borrow();
        let tree = state.tree();
        crate::shell::window_actions::focused_group(&state).and_then(|focused| {
            let workspace = tree.desktop.active_workspace()?;
            workspace.strips.iter().find_map(|strip| {
                let group = strip
                    .window_groups
                    .iter()
                    .find(|group| group.id == focused)?;
                let tab = group.tabs.iter().find(|tab| tab.id == group.active_tab)?;
                match &tab.tile {
                    crate::shell::state::TileContent::LocalApp { app_id, .. }
                        if launcher::app_id_matches_package(app_id, package_id) =>
                    {
                        Some((workspace.id, strip.id, group.id, tab.id, app_id.clone()))
                    }
                    _ => None,
                }
            })
        })
    };
    let Some((workspace, strip, group, tab, app_id)) = target else {
        return false;
    };
    let mut state = chrome.borrow_mut();
    let accepted =
        crate::shell::peer_windows::reopen_local_restore(&mut state.tree_mut(), tab, &app_id);
    if accepted {
        state.select_tab(workspace, strip, group, tab);
    }
    accepted
}

fn launch_resolved_installed_app(
    chrome: &Chrome,
    app: &launcher::InstalledApp,
) -> Result<(), ActionError> {
    let package_id = app.package_id.as_str();
    let display_name = app.display_name.as_str();
    grant_user_home_to_file_manager(chrome, package_id);
    if reopen_focused_local_card(chrome, package_id) {
        return Ok(());
    }
    // If the room already has this app open, focus it rather than starting
    // a second copy. See `peer_windows::opened_for_the_room`. A Terminal
    // asked for into a group (the tab strip's "+", Super+T) is the explicit
    // request for another shell, so it always launches; so does an app whose
    // process here opens more windows: asked for again, it opens one.
    let wants_another_terminal = package_id == crate::shell::window_actions::TERMINAL_PACKAGE
        && matches!(chrome.borrow().open_target, OpenTarget::Here(_));
    let opens_another_window = chrome
        .borrow()
        .tree()
        .desktop
        .active_workspace()
        .is_some_and(|workspace| launcher::opens_more_windows(package_id, workspace.room()));
    let already = if wants_another_terminal || opens_another_window {
        None
    } else {
        let st = chrome.borrow();
        let tree = st.tree();
        let salt = tree.local_salt();
        tree.desktop
            .active_workspace()
            .and_then(|ws| {
                crate::shell::peer_windows::opened_for_the_room(ws, package_id, salt)
                    .or_else(|| {
                        (package_id == "so.tilo.terminal")
                            .then(|| {
                                crate::shell::peer_windows::opened_locally_in_workspace(
                                    ws, package_id, salt,
                                )
                            })
                            .flatten()
                    })
                    .map(|(strip, group, tab)| (ws.id, strip, group, tab))
            })
            .or_else(|| {
                (package_id == SETTINGS_PACKAGE_ID)
                    .then(|| crate::shell::peer_windows::opened_here(&tree, package_id))
                    .flatten()
            })
    };
    if let Some((ws_id, strip, group, tab)) = already {
        chrome.borrow_mut().select_tab(ws_id, strip, group, tab);
        return Ok(());
    }
    // The app is already being installed/launched. Repeated activation should
    // be idempotent: one worker, one eventual window.
    if catalogue::pending_launch(package_id) {
        return Ok(());
    }
    // No "Opening…" notification: the person just asked for this app, and
    // its place on the strip — the opening card, held from this frame,
    // then its first frame — is the feedback. Settled by the window
    // appearing (`settle_launches_of_opened_windows`) or by the launch
    // failing.
    catalogue::note_pending_launch(package_id);
    // Scoped so the borrow is released before the error branch below
    // takes a mutable one.
    let held = {
        let st = chrome.borrow();
        // Decided here, while selection and layout are still true — the
        // window itself maps seconds later, on another process.
        let placement = crate::shell::placement::placement_for_target(
            &st.tree(),
            st.strip_viewport(),
            st.open_target,
        );
        crate::shell::tab_binding::hold_place_for_launch(
            &mut st.tree_mut(),
            package_id,
            display_name,
            placement,
        )
    };
    // The person opening the app is its author. The queued launch copies
    // the capability list now, so the grant has to be in that copy.
    let launched = {
        let st = chrome.borrow();
        let tree = st.tree();
        let room = tree
            .desktop
            .active_workspace()
            .map(crate::shell::state::Workspace::room);
        drop(tree);
        room.map(|room| {
            launcher::queue_native_authoring_launch(
                app.clone(),
                st.tree_handle(),
                room,
                st.native_app_opening.clone(),
            )
        })
    };
    // Non-blocking past resolution — only pre-fork failures land here, the
    // rest arrive as launch events drained each frame.
    let outcome = launched.unwrap_or(Err(launcher::LaunchError::NoWorkspace));
    // How a caller that cannot see the screen learns the launch failed.
    outcome.map_err(|err| {
        // Refused before any process: there is no window to wait for, so
        // the held place goes again and the failure notice says why.
        crate::shell::tab_binding::remove_tab(&mut chrome.borrow().tree_mut(), held);
        report_refused_launch(chrome, package_id, display_name, &err)
    })
}

impl Default for ApplicationsProvider {
    fn default() -> Self {
        Self::new()
    }
}

/// Whether an installed app's row (its id the package id) opens the app
/// whose windows announce `app_id`.
fn package_row_opens(result: &SearchResult, app_id: &str) -> bool {
    result.kind == ResourceKind::Application
        && crate::shell::launcher::app_id_matches_package(app_id, &result.id)
}

/// The row that uninstalls `result`'s app (or takes it off the person's
/// apps), listed under "More actions" on an app's row. Choosing it asks the
/// person in the shell's sheet, as Files and the Store do: one action from
/// every door ([`crate::shell::app_list`]). `None` for an app that comes
/// with the system and stays, or one the list has not read yet.
pub(crate) fn uninstall_row(provider: &'static str, result: &SearchResult) -> Option<SearchResult> {
    use os_actions::AppRemoval;
    let item = crate::shell::app_list::find(&result.id)?;
    let label = match item.removal() {
        AppRemoval::Uninstall | AppRemoval::UninstallWithData => t("search-apps-uninstall"),
        AppRemoval::RemoveFromApps => t("search-apps-remove-from-apps"),
        AppRemoval::None => return None,
    };
    let id = item.id;
    let mut row = SearchResult::nullary_fallible(
        ResourceKind::Action,
        provider,
        format!("{UNINSTALL_PREFIX}{id}"),
        label,
        None,
        result.score,
        move |_doc, _root, _chrome: &Chrome| {
            crate::shell::app_list::ask(&id);
            Ok(())
        },
    );
    row.operation_kind = tilo_actions::OperationKind::Effect;
    Some(row)
}

/// The id prefix of [`uninstall_row`]'s rows.
pub(crate) const UNINSTALL_PREFIX: &str = "uninstall:";

impl SearchProvider for ApplicationsProvider {
    fn id(&self) -> &'static str {
        PROVIDER_ID
    }
    fn secondary_actions(&self, result: &SearchResult) -> Vec<SearchResult> {
        if result.kind != ResourceKind::Application || result.id.contains(':') {
            return Vec::new();
        }
        uninstall_row(PROVIDER_ID, result).into_iter().collect()
    }
    fn opens_app(&self, result: &SearchResult, app_id: &str) -> bool {
        package_row_opens(result, app_id)
    }
    fn app_row(&self, app_id: &str) -> Option<SearchResult> {
        let snapshot = self.snapshot()?;
        let app = snapshot.entries.iter().find_map(|entry| match entry {
            ApplicationEntry::Installed(app)
                if crate::shell::launcher::app_id_matches_package(app_id, &app.package_id) =>
            {
                Some(Arc::clone(app))
            }
            _ => None,
        })?;
        self.installed_rows("", vec![app], &[Some(0)]).pop()
    }
    fn name(&self) -> &str {
        APPLICATIONS_KIND
    }
    fn resource_kinds(&self) -> &[ResourceKind] {
        &self.kinds
    }
    fn launcher_section(&self) -> Option<crate::shell::search::launcher::LauncherSection> {
        Some(crate::shell::search::launcher::LauncherSection::Apps)
    }
    fn query(&self, ctx: SearchContext<'_>) -> Vec<SearchResult> {
        if !ctx
            .wanted
            .iter()
            .any(|k| matches!(k, ResourceKind::Application))
        {
            return Vec::new();
        }
        // Compatibility for direct unit/headless callers that have not yet
        // joined the registry source-refresh protocol. Interactive palette
        // pages always run `search_revision` first (see `SearchRegistry`).
        let _ = self.search_revision(&ctx);
        self.query_page(ctx, None, SearchBudget::PALETTE).results
    }

    fn query_incomplete_reason(
        &self,
        _ctx: &SearchContext<'_>,
    ) -> Option<crate::shell::search::QueryIncompleteReason> {
        if self.index.building() {
            Some(crate::shell::search::QueryIncompleteReason::Pending)
        } else if self.snapshot().is_none() {
            Some(crate::shell::search::QueryIncompleteReason::Unavailable)
        } else {
            None
        }
    }

    fn refresh_search_index(&self, _ctx: &SearchContext<'_>) {
        self.index
            .refresh(|| self.source_generation(), self.builder());
    }

    /// The build runs off the keystroke; until its snapshot for the current
    /// sources lands, an answer from here is empty rather than complete.
    fn index_settling(&self, _ctx: &SearchContext<'_>) -> bool {
        self.snapshot().is_none()
    }

    fn search_revision(&self, _ctx: &SearchContext<'_>) -> String {
        self.revision()
    }

    /// An address is looked up in the whole snapshot. An empty query's first
    /// page holds [`SearchBudget::PALETTE`] rows, fewer than a desktop has
    /// apps, so resolving through it left every later app unreachable by id.
    fn resolve(&self, id: &str, ctx: SearchContext<'_>) -> Option<SearchResult> {
        if !ctx.wanted.contains(&ResourceKind::Application) {
            return None;
        }
        if self.snapshot().is_none() {
            self.index.settle(
                crate::shell::search::RESOLVE_INDEX_WAIT,
                || self.source_generation(),
                || self.builder(),
            );
        }
        let snapshot = self.snapshot()?;
        if let Some(package_id) = id.strip_prefix(OPEN_WITH_PREFIX) {
            return snapshot.entries.iter().find_map(|entry| match entry {
                ApplicationEntry::Installed(app) if app.package_id == package_id => {
                    Some(open_with_row(Arc::clone(app)))
                }
                _ => None,
            });
        }
        snapshot
            .entries
            .iter()
            .filter(|entry| entry.is_addressed_by(id))
            .find_map(|entry| {
                self.entry_row(&ctx, &snapshot, entry, provider_score(0))
                    .filter(|row| row.id == id)
            })
    }

    fn query_page(
        &self,
        ctx: SearchContext<'_>,
        cursor: Option<&SearchCursor>,
        budget: SearchBudget,
    ) -> SearchPage {
        if !ctx.wanted.contains(&ResourceKind::Application)
            || budget.max_results == 0
            || budget.max_work == 0
        {
            return SearchPage::exhausted(self.revision(), Vec::new());
        }
        let revision = self.revision();
        let Some(snapshot) = self.snapshot() else {
            return SearchPage::exhausted(revision, Vec::new());
        };
        let mut page = crate::shell::search::index_page(
            revision,
            cursor,
            ctx.needle,
            budget,
            |page| snapshot.index.search(ctx.needle, page),
            |hit| {
                let entry = usize::try_from(hit.key)
                    .ok()
                    .and_then(|at| snapshot.entries.get(at))?;
                let score = provider_score(hit.rank.unwrap_or(0));
                self.entry_row(&ctx, &snapshot, entry, score)
            },
        );
        // Equal matches: what is opened most, and most lately, comes first.
        // Then the order the product gives apps: "apps" names the Store and
        // "Windows Apps" alike, and the Store is what people mean.
        crate::shell::search::usage::order_ties_by(&mut page.results, |row| {
            ui::search::system::starting_point_rank(&row.id)
        });
        page
    }
}

impl ApplicationEntry {
    /// Whether the row this entry contributes is addressed by `id`, decided
    /// without building the row.
    fn is_addressed_by(&self, id: &str) -> bool {
        match self {
            Self::Installed(app) => app.package_id == id,
            Self::Dynamic(record) => record.id == id,
        }
    }
}

impl ApplicationsProvider {
    /// The one row `entry` contributes to a page, built exactly as a search
    /// would build it.
    fn entry_row(
        &self,
        ctx: &SearchContext<'_>,
        snapshot: &ApplicationsSnapshot,
        entry: &ApplicationEntry,
        score: i32,
    ) -> Option<SearchResult> {
        let score = [Some(score)];
        let mut rows = match entry {
            ApplicationEntry::Installed(app) => {
                self.installed_rows(ctx.needle, vec![Arc::clone(app)], &score)
            }
            ApplicationEntry::Dynamic(record) => self.dynamic_results_for_membership(
                vec![record.clone()],
                &score,
                &snapshot.installed_ids,
            ),
        };
        rows.pop()
    }

    fn revision(&self) -> String {
        let (installed, dynamic, entries) = self.source_generation();
        let ready = self.index.is_ready(&(installed, dynamic, entries));
        format!(
            "{}:{installed}:{dynamic}:{entries}",
            if ready { "ready" } else { "indexing" }
        )
    }
}

pub(crate) use ui::search::system::PACKAGE_ID_WEIGHT;

/// Applications match words spread over their fields, and typos in names.
const APPLICATION_MATCHING: Matching = Matching::LOOSE;

/// Each app's provider score for `needle`, for tests building rows from a
/// bare list.
#[cfg(test)]
fn test_scores(
    needle: &str,
    apps: impl Iterator<Item = Vec<(String, tilo_search::FieldWeight)>>,
) -> Vec<Option<i32>> {
    let apps: Vec<_> = apps.collect();
    ui::search::ListIndex::new(APPLICATION_MATCHING)
        .ranks(
            needle,
            apps.iter()
                .map(|row| row.iter().map(|(text, weight)| (text.as_str(), *weight))),
        )
        .into_iter()
        .map(|rank| rank.map(provider_score))
        .collect()
}

/// An app's searchable fields: [`ui::search::system::application_fields`] plus every other word
/// it is known by — the keywords its manifest declares and the ones the
/// generated first-party registry knows for its id (an app the store offers
/// but this machine does not have yet declares none locally) — and the
/// initials of its name.
pub(super) fn searchable_fields(
    name: &str,
    package_id: &str,
    category: Option<&str>,
    description: Option<&str>,
    other_names: impl IntoIterator<Item = String>,
    keywords: &[String],
) -> Vec<(String, tilo_search::FieldWeight)> {
    // The registry's names in other languages are the name too: a German
    // desktop types "Dateien" for Files whether its manifest is local or not.
    let mut other_names: Vec<String> = other_names.into_iter().collect();
    for name in tilo_desktop_search::availability::names_for(package_id) {
        if !other_names
            .iter()
            .any(|known| known.eq_ignore_ascii_case(name))
        {
            other_names.push((*name).to_owned());
        }
    }
    let registry = tilo_desktop_search::availability::keywords_for(package_id);
    ui::search::system::application_search_fields(
        name,
        package_id,
        category,
        description,
        other_names.iter().map(String::as_str),
        keywords
            .iter()
            .map(String::as_str)
            .chain(registry.iter().copied()),
    )
}

/// An installed app's [`searchable_fields`], plus its name in every
/// language its manifest gives one, each weighed as the name itself: a
/// German desktop types "Dateien", an English habit types "Files".
fn installed_fields(app: &InstalledApp) -> Vec<(String, tilo_search::FieldWeight)> {
    searchable_fields(
        &app.display_name,
        &app.package_id,
        app.category.as_deref(),
        None,
        app.localized_names.values().cloned(),
        &app.keywords,
    )
}

/// Index each entry's [`searchable_fields`], keyed by position.
fn index_entries(entries: &[ApplicationEntry]) -> SearchIndex {
    fn app_fields(document: Document, name: &str, package_id: &str) -> Document {
        searchable_fields(name, package_id, None, None, [], &[])
            .into_iter()
            .fold(document, |document, (text, weight)| {
                document.field(text, weight)
            })
    }
    let mut index =
        SearchIndex::new(APPLICATION_MATCHING).with_ranks(ui::search::system::application_ranks);
    index.replace_all(entries.iter().enumerate().map(|(at, entry)| {
        let document = Document::new(at as u64);
        match entry {
            ApplicationEntry::Installed(app) => installed_fields(app)
                .into_iter()
                .fold(document, |document, (text, weight)| {
                    document.field(text, weight)
                }),
            ApplicationEntry::Dynamic(record) => app_fields(document, &record.name, &record.id),
        }
    }));
    if let Err(error) = index.commit() {
        tracing::error!(%error, "could not index applications");
    }
    index
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::shell::chrome_state::ChromeState;
    use crate::shell::state::{WorkspaceTree, WorkspaceTreeHandle};
    use automerge::AutoCommit;
    use std::path::PathBuf;
    use std::sync::Arc;

    fn chrome_rc() -> Chrome {
        let handle: WorkspaceTreeHandle =
            crate::shell::state::WorkspaceTreeHandle::new(WorkspaceTree::default());
        ChromeState::new_with_handle(handle)
    }

    /// Files' home grant lasts one session; its authorship of the workspace
    /// persists. So in any session after the first, opening Files adds the
    /// home grant and no authorship grant, and nothing else saves before the
    /// launch, which is released only once the accepted policy (published by
    /// a save) holds the grant. The grant itself must queue that save, or
    /// every open of Files is refused for the rest of the session.
    #[test]
    fn the_files_home_grant_queues_the_save_that_publishes_it() {
        let home = tilo_test_env::TestHome::new().unwrap();
        let state = home.state().join("files-home-grant").join("state.json");
        let mut tree = WorkspaceTree::bootstrap_default();
        tree.persist_path = Some(state.clone());
        let workspace = tree.desktop.active;
        crate::shell::persist::wait_for_saves();
        assert!(!state.exists(), "nothing saved the tree before the grant");
        assert!(grant_file_manager_home(
            &mut tree,
            workspace,
            SYSTEM_FILE_MANAGER
        ));
        crate::shell::persist::wait_for_saves();
        assert!(
            state.exists(),
            "the grant queued a save, whose publication the launch waits for"
        );
        assert!(
            !grant_file_manager_home(&mut tree, workspace, SYSTEM_FILE_MANAGER),
            "a second open finds the grant in place"
        );
    }

    #[test]
    fn catalogue_completion_waits_for_actual_worker() {
        use crate::shell::chrome_state::SearchScope;
        use crate::shell::search::action_api::{ActionCatalog, AllVisible};
        use crate::shell::search::{QueryArgs, QueryIncompleteReason, SearchRegistry};
        let source = ApplicationsTestSource::new(Vec::new(), Vec::new());
        let gate = source.state.lock().expect("hold source materialization");
        let (woken, wake) = std::sync::mpsc::channel();
        let _installed = crate::shell::search::invocation_wake::install(Arc::new(move || {
            woken.send(()).expect("waiting catalogue caller");
        }));
        let mut registry = SearchRegistry::new();
        registry.register(std::rc::Rc::new(ApplicationsProvider::with_test_source(
            source.clone(),
        )));
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
        wake.recv_timeout(tilo_test_env::hang_bound())
            .expect("the actual index worker must wake a held listing");
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
    fn opening_a_live_local_terminal_focuses_without_another_record_writer() {
        use crate::shell::state::{
            Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth, Workspace, WorkspaceId,
            next_id,
        };
        let tab_id = next_id();
        let mut workspace = Workspace::new(WorkspaceId::from(next_id()), "Home");
        let mut strip = Strip::new(StripId::from(next_id()), "Main");
        let group_id = next_id();
        strip.window_groups.push(WindowGroup {
            id: group_id,
            tabs: vec![Tab {
                id: tab_id,
                title: "Terminal".into(),
                tile: TileContent::Foreign {
                    app_id: "so.tilo.terminal".into(),
                    surface_id: 7,
                },
                app_title: None,
            }],
            active_tab: tab_id,
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        });
        workspace.strips.push(strip);
        let mut tree = WorkspaceTree::default();
        tree.set_salt_override(crate::shell::state::peer_salt_of(tab_id));
        tree.desktop.active = workspace.id;
        tree.desktop.workspaces = vec![workspace];
        let handle = crate::shell::state::WorkspaceTreeHandle::new(tree);
        let chrome = ChromeState::new_with_handle(handle.clone());
        let provider = ApplicationsProvider::new();
        let rows = provider.results_for("terminal", vec![app("so.tilo.terminal", "Terminal")]);
        let row = rows.first().expect("installed Terminal action");
        let mut doc = AutoCommit::new();
        (row.on_open)(
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
            &chrome,
            &ActionArgs::none(),
        )
        .expect("Open must focus the existing local Terminal");
        assert_eq!(chrome.borrow().active_tab_id(), Some(tab_id));
        assert!(
            chrome.borrow().notifications().is_empty(),
            "focus must not queue a second launcher or opening notice"
        );
        assert_eq!(
            handle.read().desktop.workspaces[0].strips[0].window_groups[0]
                .tabs
                .len(),
            1
        );
    }

    /// The group's "+" asks for a tab in that group: another Terminal opens
    /// even while one is live, where the ordinary open only focuses it.
    #[test]
    #[serial_test::serial]
    fn opening_a_terminal_into_a_group_launches_another_instead_of_focusing() {
        use crate::shell::state::{
            Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth, Workspace, WorkspaceId,
            next_id,
        };
        let tab_id = next_id();
        let mut workspace = Workspace::new(WorkspaceId::from(next_id()), "Home");
        let mut strip = Strip::new(StripId::from(next_id()), "Main");
        let group_id = next_id();
        strip.window_groups.push(WindowGroup {
            id: group_id,
            tabs: vec![Tab {
                id: tab_id,
                title: "Terminal".into(),
                tile: TileContent::Foreign {
                    app_id: "so.tilo.terminal".into(),
                    surface_id: 7,
                },
                app_title: None,
            }],
            active_tab: tab_id,
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        });
        let mut tree = WorkspaceTree::default();
        tree.set_salt_override(crate::shell::state::peer_salt_of(tab_id));
        tree.desktop.active = workspace.id;
        tree.desktop.workspaces = vec![workspace];
        let handle = crate::shell::state::WorkspaceTreeHandle::new(tree);
        // Apps open in a room a host holds and this device's native owner
        // entered: the workspace's room is placed on this device's host.
        let _hosted = crate::shell::test_support::host_active_room(&handle);
        handle
            .write()
            .desktop
            .active_workspace_mut()
            .expect("the workspace")
            .strips
            .push(strip);
        let chrome = ChromeState::new_with_handle(handle);
        let rows = ApplicationsProvider::new()
            .results_for("terminal", vec![app("so.tilo.terminal", "Terminal")]);
        chrome.borrow_mut().open_target = OpenTarget::Here(group_id);
        let mut doc = AutoCommit::new();
        let _ = (rows[0].on_open)(
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
            &chrome,
            &ActionArgs::none(),
        );
        // Opening a Terminal into a group launches another instance: it is
        // pending until its window appears. A launch the person can see is
        // confirmed in passing rather than with a notice, so the pending
        // launch is what tells it from a silent refocus of the open one.
        assert!(
            catalogue::take_pending_launch("so.tilo.terminal"),
            "opening a Terminal into a group must launch another, not refocus the open one"
        );
        launcher::clear_pending_placements();
    }

    #[test]
    fn opening_the_focused_failed_app_reuses_its_saved_window() {
        use crate::shell::state::{
            Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth, Workspace, WorkspaceId,
            next_id,
        };
        let tab = next_id();
        let mut workspace = Workspace::new(WorkspaceId::from(next_id()), "Home");
        let mut strip = Strip::new(StripId::from(next_id()), "Main");
        strip.window_groups.push(WindowGroup {
            id: next_id(),
            tabs: vec![Tab {
                id: tab,
                title: "Browser".into(),
                tile: TileContent::LocalApp {
                    app_id: "so.tilo.browser".into(),
                    label: "Browser".into(),
                    note: "Failed to open".into(),
                },
                app_title: None,
            }],
            active_tab: tab,
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        });
        workspace.strips.push(strip);
        let mut tree = WorkspaceTree::default();
        tree.set_salt_override(crate::shell::state::peer_salt_of(tab));
        tree.desktop.active = workspace.id;
        tree.desktop.workspaces = vec![workspace];
        tree.remember_local_app_record(tab, 8183);
        let records = tree.local_app_records.clone();
        let handle = crate::shell::state::WorkspaceTreeHandle::new(tree);
        let chrome = ChromeState::new_with_handle(handle.clone());
        let rows = ApplicationsProvider::new()
            .results_for("browser", vec![app("so.tilo.browser", "Browser")]);
        let mut doc = AutoCommit::new();
        (rows[0].on_open)(
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
            &chrome,
            &ActionArgs::none(),
        )
        .expect("retry the focused saved window");
        (rows[0].on_open)(
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
            &chrome,
            &ActionArgs::none(),
        )
        .expect("repeated activation while the saved recovery is queued");
        let state = handle.read();
        let tabs = &state.desktop.workspaces[0].strips[0].window_groups[0].tabs;
        assert_eq!(tabs.len(), 1, "Applications must not create another tab");
        assert_eq!(tabs[0].id, tab);
        assert!(matches!(&tabs[0].tile, TileContent::LocalApp { note, .. } if note.is_empty()));
        assert_eq!(state.local_app_records, records);
        drop(state);
        assert_eq!(chrome.borrow().active_tab_id(), Some(tab));
        assert!(chrome.borrow().notifications().is_empty());
        {
            let mut state = handle.write();
            let TileContent::LocalApp { note, .. } =
                &mut state.desktop.workspaces[0].strips[0].window_groups[0].tabs[0].tile
            else {
                panic!("the saved card remains local");
            };
            *note = crate::shell::peer_windows::OPENING.into();
            drop(state);
        }
        (rows[0].on_open)(
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
            &chrome,
            &ActionArgs::none(),
        )
        .expect("opening recovery is already accepted");
        let state = handle.read();
        assert!(
            matches!(&state.desktop.workspaces[0].strips[0].window_groups[0].tabs[0].tile,
            TileContent::LocalApp { note, .. } if note == crate::shell::peer_windows::OPENING)
        );
        assert_eq!(state.local_app_records, records);
        drop(state);
        assert!(chrome.borrow().notifications().is_empty());
    }

    fn refresh_until_ready(provider: &ApplicationsProvider, ctx: &SearchContext<'_>) {
        tilo_test_env::wait_until("the application search index to become ready", || {
            provider.refresh_search_index(ctx);
            provider.search_revision(ctx).starts_with("ready:")
        });
    }

    #[test]
    fn an_app_taken_off_the_persons_apps_is_not_listed() {
        let installed = [app("so.tilo.notes", "Notes"), app("so.tilo.chess", "Chess")];
        let snapshot = ApplicationsProvider::snapshot_from_parts(
            &installed,
            Vec::new(),
            &["so.tilo.notes".to_owned()],
        );
        let listed: Vec<&str> = snapshot
            .entries
            .iter()
            .map(|entry| match entry {
                ApplicationEntry::Installed(app) => app.package_id.as_str(),
                ApplicationEntry::Dynamic(_) => "",
            })
            .collect();
        assert_eq!(listed, ["so.tilo.chess"]);
    }

    fn app(package_id: &str, display_name: &str) -> InstalledApp {
        app_in(package_id, display_name, None)
    }

    fn app_in(package_id: &str, display_name: &str, category: Option<&str>) -> InstalledApp {
        InstalledApp {
            package_id: package_id.to_string(),
            display_name: display_name.to_string(),
            localized_names: BTreeMap::default(),
            keywords: Vec::new(),
            category: category.map(str::to_string),
            version: "0.1.0".to_string(),
            manifest_path: PathBuf::from("/apps/x/manifest.toml"),
            payload_root: PathBuf::from("/apps/x"),
            has_gui: true,
            opens_extensions: Vec::new(),
            opens_mime_types: Vec::new(),
            opens_uri_schemes: Vec::new(),
            opens_secondary: false,
            offers: Vec::new(),
        }
    }

    #[test]
    fn activating_an_app_is_idempotent_and_reports_failures() {
        // Phase `repeated_activation_while_opening_is_idempotent`.
        {
            let chrome = chrome_rc();
            let package_id = "so.tilo.repeated-open-fixture";
            catalogue::note_pending_launch(package_id);

            assert!(launch_installed_app(&chrome, package_id, "Fixture").is_ok());
            assert!(
                chrome.borrow().notifications().is_empty(),
                "a second activation must not start another worker or say anything"
            );
            assert!(catalogue::take_pending_launch(package_id));
        }

        // Phase `a_failed_launch_notifies_the_user_and_refuses_the_caller`.
        // A launch that cannot start must reach both audiences: a notification
        // for the screen, and a refusal for the caller that invoked the row by
        // address — without it, a script is told the app opened and waits for
        // a window that never arrives. Drives the real `on_open` closure.
        {
            use crate::shell::search::action_api::ActionError;

            let p = ApplicationsProvider::new();
            let c = chrome_rc();
            let mut doc = AutoCommit::new();
            // The registry does not actually contain this app, so launch refuses it.
            let out = p.results_for("ghost", vec![app("so.tilo.ghost", "Ghost")]);
            let row = out.first().expect("a row for the ghost app");
            assert!(c.borrow().notifications().is_empty());
            let outcome = (row.on_open)(
                &mut ui::widget::Edits::lend(&mut doc),
                &automerge::ROOT,
                &c,
                &crate::shell::search::action_api::ActionArgs::none(),
            );

            let err = outcome.expect_err("a launch that cannot start must not report success");
            assert!(
                matches!(err, ActionError::Refused(_)),
                "a launch failure is a refusal, not an argument error: {err:?}"
            );
            assert_eq!(
                err.code(),
                "action_failed",
                "the wire code must say the action ran and could not do its job"
            );
            assert!(
                err.to_string().contains("Ghost"),
                "the refusal must name what failed to open: {err}"
            );

            let st = c.borrow();
            let n = st
                .notifications()
                .last()
                .expect("a failed launch must notify the user");
            assert_eq!(n.title, "Couldn't open Ghost");
            assert_eq!(n.severity, NotificationSeverity::Error);
            assert!(
                !n.body.is_empty(),
                "the notification must say why the launch failed"
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn application_matching_ranks_name_then_id_then_category() {
        // Phase `matching_app_offers_an_open_row`.
        {
            let p = ApplicationsProvider::new();
            let out = p.results_for("hel", vec![app("so.tilo.hello", "Hello")]);
            assert_eq!(out.len(), 1);
            assert_eq!(out[0].title, "Hello", "the title is the app's name");
            assert_eq!(out[0].id, "so.tilo.hello");
            assert_eq!(out[0].score, 100, "prefix match on the display name");
            assert_eq!(
                out[0].subtitle.as_deref(),
                None,
                "no vendor, package id or version under an app's name"
            );
        }

        // Phase `declared_category_is_matchable_and_replaces_the_version_subtitle`.
        {
            let p = ApplicationsProvider::new();
            let out = p.results_for(
                "office",
                vec![
                    app_in("so.example.ledger", "Ledger", Some("Office")),
                    app_in("so.example.abacus", "Abacus", Some("Utility")),
                ],
            );
            assert_eq!(out.len(), 1, "only the Office app matched: {out:?}");
            assert_eq!(out[0].id, "so.example.ledger");
            assert_eq!(out[0].score, 34, "category ranks below name and id matches");
            assert_eq!(
                out[0].subtitle.as_deref(),
                Some("Office"),
                "with no tag line, the declared category is the only metadata a row shows"
            );
        }

        // Phase `a_tilo_apps_tag_line_is_its_second_line`.
        {
            let p = ApplicationsProvider::new();
            let out = p.results_for(
                "sheets",
                vec![app_in("so.tilo.sheets", "Sheets", Some("Office"))],
            );
            let tagline = app_catalogue::fleet::metadata("so.tilo.sheets")
                .map(|app| app.tagline)
                .expect("Sheets is in the fleet");
            assert_eq!(
                out[0].subtitle.as_deref(),
                Some(tagline),
                "every app row shows its tag line as the second line"
            );
        }

        // Phase `category_never_outranks_a_name_or_id_match`.
        {
            let p = ApplicationsProvider::new();
            // "system" is Monitor's category AND a substring of the other
            // app's display name. The name match has to win.
            let out = p.results_for(
                "system",
                vec![
                    app_in("so.example.monitor", "Monitor", Some("System")),
                    app_in("so.example.sysinfo", "System Info", Some("Utility")),
                ],
            );
            assert_eq!(out.len(), 2);
            let score = |id: &str| out.iter().find(|r| r.id == id).unwrap().score;
            assert_eq!(
                score("so.example.sysinfo"),
                100,
                "prefix on the display name"
            );
            assert_eq!(score("so.example.monitor"), 34, "category-only match");
            assert!(score("so.example.sysinfo") > score("so.example.monitor"));
        }

        // Phase `package_id_matches_score_below_name_matches`.
        {
            let p = ApplicationsProvider::new();
            let out = p.results_for(
                "so.tilo",
                vec![app("so.tilo.hello", "Hello"), app("org.x.zed", "Zed")],
            );
            assert_eq!(out.len(), 1, "only the matching package: {out:?}");
            // A shared id *prefix* names a publisher, not an app, so it
            // stays below a name match.
            assert_eq!(out[0].score, 93);
            let by_name = p.results_for("hello", vec![app("so.tilo.hello", "Hello")]);
            assert!(
                by_name[0].score > out[0].score,
                "naming the app must still beat naming its publisher"
            );
        }

        // Phase `the_id_last_segment_is_an_identity_match`.
        // The display name is "Hex Editor", which never prefix-matches the id.
        {
            let p = ApplicationsProvider::new();
            let out = p.results_for("hex-editor", vec![app("so.tilo.hex-editor", "Hex Editor")]);
            assert_eq!(out.len(), 1, "the app must match at all: {out:?}");
            assert_eq!(
                out[0].score, 100,
                "naming an app's id segment identifies it as exactly as \
                 naming the app does"
            );
            let full = p.results_for(
                "so.tilo.hex-editor",
                vec![app("so.tilo.hex-editor", "Hex Editor")],
            );
            assert_eq!(full[0].score, 100);
        }

        // Phase `non_matching_needle_returns_nothing`.
        {
            let p = ApplicationsProvider::new();
            let out = p.results_for("zzzqqq", vec![app("so.tilo.hello", "Hello")]);
            assert!(out.is_empty());
        }

        // Phase `unrequested_kind_returns_empty`.
        {
            let p = ApplicationsProvider::new();
            let c = chrome_rc();
            let cs = c.borrow();
            let doc = AutoCommit::new();
            let out = p.query(SearchContext {
                query_scope: None,
                include_programmatic: false,
                needle: "hello",
                wanted: &[ResourceKind::File],
                chrome: &cs,
                doc: &doc,
                doc_root: &automerge::ROOT,
            });
            assert!(out.is_empty());
        }
    }

    #[test]
    fn reviewed_shortcuts_are_searchable_and_malformed_records_are_hidden() {
        // Phase `reviewed_web_shortcut_is_searchable_but_malformed_records_are_hidden`.
        {
            let p = ApplicationsProvider::new();
            let record = |id: &str, target: Option<&str>| DynamicLauncherRecord {
                id: id.into(),
                owner_app_id: "org.example.creator".into(),
                name: "Project board".into(),
                launcher_type: 2,
                target: target.map(str::to_string),
                icon_kind: "tilo-web".into(),
                icon_bytes: None,
            };
            let out = p.dynamic_results_for(
                "project",
                vec![
                    record("portal-valid", Some("https://example.test/board")),
                    record("portal-invalid", Some("javascript:alert(1)")),
                ],
                &[],
            );
            assert_eq!(out.len(), 1);
            assert_eq!(out[0].id, "portal-valid");
            assert_eq!(out[0].title, "Project board");
            assert!(out[0].subtitle.as_deref().unwrap().contains("Shortcut"));
        }
    }

    #[test]
    fn empty_and_typed_queries_list_apps_in_a_useful_order() {
        // Phase `empty_needle_lists_every_app_with_a_useful_fresh_system_order`.
        {
            let p = ApplicationsProvider::new();
            let out = p.results_for(
                "",
                vec![
                    app("so.tilo.monitor", "System Monitor"),
                    app("so.tilo.browser", "Browser"),
                    app("so.tilo.notes", "Notes"),
                    app("so.tilo.hello", "Hello"),
                    app("so.tilo.api-tester", "API Tester"),
                    app("dev.tilo.file-explorer", "Files"),
                ],
            );
            assert_eq!(out.len(), 6);
            let score = |id: &str| out.iter().find(|row| row.id == id).unwrap().score;
            assert!(score("so.tilo.browser") > score("so.tilo.notes"));
            assert!(score("so.tilo.notes") > score("so.tilo.monitor"));
            assert!(score("so.tilo.monitor") > score("so.tilo.hello"));
            assert!(
                score("so.tilo.hello") > score("so.tilo.api-tester"),
                "developer tools never lead a fresh palette"
            );
            assert!(
                score("dev.tilo.file-explorer") > score("so.tilo.notes"),
                "a local build's apps keep their place"
            );
            assert!(
                out.iter()
                    .filter_map(|row| row.subtitle.as_deref())
                    .all(|subtitle| !subtitle.contains("so.tilo") && !subtitle.contains("0.1.0")),
                "no package id or version on a row"
            );
        }

        // Phase `typed_search_ignores_default_presentation_tiers`.
        {
            let p = ApplicationsProvider::new();
            let out = p.results_for(
                "monitor",
                vec![
                    app("so.tilo.browser", "Browser"),
                    app("so.tilo.monitor", "System Monitor"),
                ],
            );
            assert_eq!(out.len(), 1);
            assert_eq!(out[0].id, "so.tilo.monitor");
            assert_eq!(
                out[0].score, 100,
                "normal match scoring remains authoritative"
            );
            assert!(
                !out[0]
                    .subtitle
                    .as_deref()
                    .unwrap_or_default()
                    .contains("System tools"),
                "presentation bands are not search metadata or query noise"
            );
        }
    }

    /// An app is found by its name in any language its manifest gives, so a
    /// German desktop finds Files as "Dateien" and English still works.
    #[test]
    fn an_installed_app_is_found_by_its_localized_name_and_its_own() {
        let mut files = app_in("so.tilo.file-explorer", "Files", Some("System"));
        files.localized_names.insert("de".into(), "Dateien".into());
        let p = ApplicationsProvider::with_test_source(ApplicationsTestSource::new(
            vec![files.clone()],
            Vec::new(),
        ));
        let c = chrome_rc();
        let state = c.borrow();
        let doc = AutoCommit::new();
        for needle in ["Dateien", "dateien", "Files"] {
            let ctx = SearchContext {
                query_scope: None,
                include_programmatic: false,
                needle,
                wanted: &[ResourceKind::Application],
                chrome: &state,
                doc: &doc,
                doc_root: &automerge::ROOT,
            };
            refresh_until_ready(&p, &ctx);
            let results = p.query(ctx);
            assert!(
                results
                    .iter()
                    .any(|row| row.id.ends_with("so.tilo.file-explorer")),
                "{needle:?} finds Files: {:?}",
                results.iter().map(|row| &row.title).collect::<Vec<_>>()
            );
        }
        assert_eq!(files.name_for("de-DE"), "Dateien");
        assert_eq!(files.name_for("de"), "Dateien");
        assert_eq!(files.name_for("en-US"), "Files");
    }

    /// `tilo open notes ~/a.md` reaches the app through this address: one
    /// typed `open` list, resolved by address and never listed, so the
    /// palette's own row for the app stays nullary.
    #[test]
    fn open_with_is_addressed_not_listed_and_requires_the_list() {
        let notes = app_in("so.tilo.notes", "Notes", Some("Productivity"));
        let p = ApplicationsProvider::with_test_source(ApplicationsTestSource::new(
            vec![notes],
            Vec::new(),
        ));
        let c = chrome_rc();
        let state = c.borrow();
        let doc = AutoCommit::new();
        let ctx = SearchContext {
            query_scope: None,
            include_programmatic: false,
            needle: "notes",
            wanted: &[ResourceKind::Application],
            chrome: &state,
            doc: &doc,
            doc_root: &automerge::ROOT,
        };
        refresh_until_ready(&p, &ctx);
        let listed = p.query(ctx);
        assert!(listed.iter().any(|row| row.id == "so.tilo.notes"));
        assert!(
            listed
                .iter()
                .all(|row| !row.id.starts_with(OPEN_WITH_PREFIX) && row.params.is_empty()),
            "the palette row stays nullary: {listed:?}"
        );
        let row = p
            .resolve("open-with:so.tilo.notes", ctx)
            .expect("an installed app resolves by its open-with address");
        assert_eq!(row.title, "Notes");
        assert_eq!(row.params.len(), 1);
        assert_eq!(row.params[0].name, "open");
        assert!(row.params[0].required);
        assert!(
            p.resolve("open-with:so.tilo.absent", ctx).is_none(),
            "only an installed app opens files"
        );
        let missing = crate::shell::search::action_api::ActionArgs::bind_json(
            &row.params,
            &serde_json::Map::new(),
        );
        assert!(missing.is_err(), "the list is required");
    }

    #[test]
    fn an_app_that_is_here_is_just_opened() {
        // Phase `an_app_that_is_already_here_does_not_raise_a_loading_state`.
        // A state that flashes for a hundred milliseconds is noise, not feedback.
        {
            let p = ApplicationsProvider::new();
            let out = p.results_for("she", vec![app("so.tilo.sheets", "Sheets")]);
            let row = out.first().expect("a row for the installed app");
            assert!(
                !row.title.to_lowercase().contains("load"),
                "an app that is here is just opened: {:?}",
                row.title
            );
        }
    }

    #[test]
    fn cold_and_warm_public_pages_never_rescan_a_hundred_thousand_apps() {
        let chrome = chrome_rc();
        let chrome = chrome.borrow();
        let doc = AutoCommit::new();
        let ctx = SearchContext {
            query_scope: None,
            include_programmatic: false,
            needle: "unique-tail",
            wanted: &[ResourceKind::Application],
            chrome: &chrome,
            doc: &doc,
            doc_root: &automerge::ROOT,
        };
        let cold = ApplicationsProvider::new();
        assert!(
            cold.query_page(ctx, None, SearchBudget::PALETTE)
                .results
                .is_empty()
        );
        assert!(
            !cold.index.building(),
            "a public cold query must not start or clone a full application corpus"
        );

        // A source of its own: the retained snapshot below stays current
        // whatever other tests install or invalidate meanwhile.
        let provider = ApplicationsProvider::with_test_source(ApplicationsTestSource::new(
            Vec::new(),
            Vec::new(),
        ));
        let entries = (0..=4_000)
            .map(|index| {
                ApplicationEntry::Installed(Arc::new(app_in(
                    &format!("so.tilo.fixture-{index}"),
                    if index == 4_000 {
                        "unique-tail"
                    } else {
                        "common app"
                    },
                    None,
                )))
            })
            .collect::<Vec<_>>();
        let index = index_entries(&entries);
        provider.index.set_ready(
            provider.source_generation(),
            ApplicationsSnapshot {
                entries,
                installed_ids: HashSet::new(),
                index,
            },
        );
        let page = provider.query_page(
            SearchContext {
                query_scope: None,
                include_programmatic: false,
                needle: "unique-tail",
                wanted: &[ResourceKind::Application],
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
        assert_eq!(page.results[0].title, "unique-tail");
        assert!(page.exhausted, "the rare posting needs one warm inspection");
        assert!(!provider.index.building());
    }

    #[test]
    fn a_worker_snapshot_is_rejected_when_its_source_revision_changed() {
        let source = ApplicationsTestSource::new(Vec::new(), Vec::new());
        let provider = ApplicationsProvider::with_test_source(Arc::clone(&source));
        let chrome = chrome_rc();
        let chrome = chrome.borrow();
        let doc = AutoCommit::new();
        let ctx = SearchContext {
            query_scope: None,
            include_programmatic: false,
            needle: "anything",
            wanted: &[ResourceKind::Application],
            chrome: &chrome,
            doc: &doc,
            doc_root: &automerge::ROOT,
        };
        let initial_revision = provider.search_revision(&ctx);
        provider.refresh_search_index(&ctx);
        tilo_test_env::wait_until("the source to materialize once", || {
            source
                .materializations
                .load(std::sync::atomic::Ordering::Relaxed)
                >= 1
        });
        assert_eq!(
            source
                .materializations
                .load(std::sync::atomic::Ordering::Relaxed),
            1
        );
        source.replace_installed(vec![app("so.tilo.changed", "Changed")]);
        provider.refresh_search_index(&ctx);
        assert!(provider.search_revision(&ctx).starts_with("indexing:"));
        refresh_until_ready(&provider, &ctx);
        assert!(provider.search_revision(&ctx).starts_with("ready:"));
        assert_ne!(provider.search_revision(&ctx), initial_revision);
    }

    #[test]
    fn every_application_is_addressable_past_the_first_palette_page() {
        use crate::shell::chrome_state::SearchScope;
        use crate::shell::search::SearchRegistry;
        use crate::shell::search::action_api::{ActionCatalog, AllVisible, ResourceRef};
        use std::rc::Rc;

        let count = SearchBudget::PALETTE.max_results + 12;
        let installed = (0..count)
            .map(|index| {
                app(
                    &format!("so.tilo.fixture-{index:03}"),
                    &format!("Fixture {index:03}"),
                )
            })
            .collect();
        let source = ApplicationsTestSource::new(installed, Vec::new());
        let provider = Rc::new(ApplicationsProvider::with_test_source(source));
        let chrome = chrome_rc();
        let doc = AutoCommit::new();
        {
            let chrome = chrome.borrow();
            let ctx = SearchContext {
                query_scope: None,
                include_programmatic: false,
                needle: "",
                wanted: &[ResourceKind::Application],
                chrome: &chrome,
                doc: &doc,
                doc_root: &automerge::ROOT,
            };
            refresh_until_ready(&provider, &ctx);
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
            let resource = ResourceRef::new("applications", id);
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
        let last = format!("so.tilo.fixture-{:03}", count - 1);
        assert_eq!(
            select(&last).as_deref(),
            Some(format!("Fixture {:03}", count - 1).as_str()),
            "an installed app past the first page must still open by its address"
        );
        assert_eq!(select("so.tilo.store-only"), None);
        assert_eq!(select("so.tilo.missing"), None);
    }

    /// An agent's first request to a session is often an invocation, with no
    /// search or palette before it to take in the index build.
    #[test]
    fn an_app_opens_by_its_address_without_any_search_first() {
        use crate::shell::chrome_state::SearchScope;
        use crate::shell::search::SearchRegistry;
        use crate::shell::search::action_api::{ActionCatalog, AllVisible, ResourceRef};
        use std::rc::Rc;

        let source =
            ApplicationsTestSource::new(vec![app("so.tilo.calculator", "Calculator")], Vec::new());
        let mut registry = SearchRegistry::new();
        registry.register(Rc::new(ApplicationsProvider::with_test_source(source)));
        let chrome = chrome_rc();
        let doc = AutoCommit::new();
        let resource = ResourceRef::new("applications", "so.tilo.calculator");
        // The first request resolves: no listing beforehand, no retry.
        let opened = ActionCatalog::build_for_resource(
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
        .map(|action| action.title().to_string());
        assert_eq!(opened.as_deref(), Some("Calculator"));
    }

    #[test]
    fn source_backed_cold_query_does_no_application_index_work_before_refresh() {
        let source = ApplicationsTestSource::new(
            (0..=4_000)
                .map(|index| {
                    app(
                        &format!("so.tilo.cold-fixture-{index}"),
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
        let provider = ApplicationsProvider::with_test_source(Arc::clone(&source));
        let chrome = chrome_rc();
        let chrome = chrome.borrow();
        let doc = AutoCommit::new();
        let ctx = SearchContext {
            query_scope: None,
            include_programmatic: false,
            needle: "unique-source-tail",
            wanted: &[ResourceKind::Application],
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
                .index
                .probe
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
        assert!(!provider.index.building());

        tilo_test_env::wait_until("the application index to become ready", || {
            provider.refresh_search_index(&ctx);
            provider.search_revision(&ctx).starts_with("ready:")
        });
        assert_eq!(
            provider
                .index
                .probe
                .builds
                .load(std::sync::atomic::Ordering::Relaxed),
            1
        );
        assert_ne!(
            *provider
                .index
                .probe
                .thread
                .lock()
                .expect("index build probe lock"),
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
        assert_eq!(page.results[0].title, "unique-source-tail");
    }
}

/// Tiny session-owned catalog of apps the native host can actually construct.
/// It shares application visibility policy and action dispatch with package apps.
pub struct NativeApplicationsProvider {
    /// The apps' names, ids and keywords, indexed while they stay the same.
    ranker: ui::search::ListIndex,
}

impl Default for NativeApplicationsProvider {
    fn default() -> Self {
        Self {
            ranker: ui::search::ListIndex::new(APPLICATION_MATCHING)
                .with_ranks(ui::search::system::application_ranks),
        }
    }
}

impl SearchProvider for NativeApplicationsProvider {
    fn id(&self) -> &'static str {
        "native-applications"
    }
    fn opens_app(&self, result: &SearchResult, app_id: &str) -> bool {
        package_row_opens(result, app_id)
    }
    fn name(&self) -> &'static str {
        "Applications"
    }
    fn resource_kinds(&self) -> &[ResourceKind] {
        &[ResourceKind::Application]
    }
    fn search_revision(&self, ctx: &SearchContext<'_>) -> String {
        format!("{:?}", ctx.chrome.hosted_inputs.native_apps())
    }
    fn query(&self, ctx: SearchContext<'_>) -> Vec<SearchResult> {
        if !ctx.wanted.contains(&ResourceKind::Application) {
            return Vec::new();
        }
        let apps = ctx.chrome.hosted_inputs.native_apps();
        let fields: Vec<_> = apps
            .iter()
            .map(|app| searchable_fields(app.display_name, app.package_id, None, None, [], &[]))
            .collect();
        let ranks = self.ranker.ranks(
            ctx.needle,
            fields
                .iter()
                .map(|row| row.iter().map(|(text, weight)| (text.as_str(), *weight))),
        );
        apps.iter()
            .zip(ranks)
            .filter_map(|(app, rank)| {
                let score = provider_score(rank?);
                let id = app.package_id;
                Some(SearchResult::nullary_fallible(
                    ResourceKind::Application,
                    self.id(),
                    id,
                    app.display_name,
                    None,
                    score,
                    move |_doc, _root, chrome: &Chrome| {
                        let app = chrome
                            .borrow()
                            .hosted_inputs
                            .native_apps()
                            .iter()
                            .find(|app| app.package_id == id)
                            .cloned()
                            .ok_or_else(|| {
                                ActionError::Refused(
                                    "This application is no longer available".into(),
                                )
                            })?;
                        let mut state = chrome.borrow_mut();
                        let destination = match state.open_target {
                            OpenTarget::Beside => crate::shell::window_actions::Destination::Search,
                            OpenTarget::Here(group) => {
                                crate::shell::window_actions::Destination::InGroup(group)
                            }
                            // Right of the focused window, which is the
                            // group this target names.
                            OpenTarget::RightOf(_) => {
                                crate::shell::window_actions::Destination::Beside
                            }
                        };
                        crate::shell::window_actions::open_hosted(
                            &mut state,
                            app.kind,
                            app.display_name,
                            destination,
                        );
                        Ok(())
                    },
                ))
            })
            .collect()
    }
}

#[cfg(test)]
mod native_application_tests {
    use super::*;
    use crate::shell::{
        chrome_state::ChromeState,
        hosted_inputs::HostedTileInputs,
        state::{AppKind, WorkspaceTree},
    };

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn native_app_search_opens_the_real_hosted_app_in_the_right_place() {
        // Phase `native_files_search_opens_the_real_hosted_app_and_revalidates_availability`.
        {
            let directory = tempfile::tempdir().unwrap();
            let chrome = ChromeState::new(WorkspaceTree::bootstrap_default());
            chrome.borrow_mut().hosted_inputs =
                HostedTileInputs::for_host_directory_privately(directory.path()).unwrap();
            let mut doc = automerge::AutoCommit::new();
            let results = NativeApplicationsProvider::default().query(SearchContext {
                query_scope: None,
                include_programmatic: false,
                needle: "Files",
                wanted: &[ResourceKind::Application],
                chrome: &chrome.borrow(),
                doc: &doc,
                doc_root: &automerge::ROOT,
            });
            assert_eq!(results.len(), 1);
            assert_eq!(results[0].id, crate::shell::hosted_inputs::FILES_PACKAGE_ID);
            (results[0].on_open)(
                &mut ui::widget::Edits::lend(&mut doc),
                &automerge::ROOT,
                &chrome,
                &ActionArgs::none(),
            )
            .unwrap();
            assert_eq!(chrome.borrow().active_app_title().as_deref(), Some("Files"));
            assert!(
                chrome
                    .borrow()
                    .tree()
                    .desktop
                    .active_workspace()
                    .unwrap()
                    .strips
                    .iter()
                    .flat_map(|s| &s.window_groups)
                    .flat_map(|g| &g.tabs)
                    .any(|tab| matches!(
                        &tab.tile,
                        crate::shell::state::TileContent::AppPlaceholder {
                            kind: AppKind::FileExplorer,
                            ..
                        }
                    ))
            );
            chrome.borrow_mut().hosted_inputs = HostedTileInputs::default();
            assert!(
                (results[0].on_open)(
                    &mut ui::widget::Edits::lend(&mut doc),
                    &automerge::ROOT,
                    &chrome,
                    &ActionArgs::none()
                )
                .is_err()
            );
        }

        // Phase `native_app_search_opens_beside_the_focused_window`.
        {
            let directory = tempfile::tempdir().unwrap();
            let mut tree = WorkspaceTree::bootstrap_demo();
            for title in ["second", "third", "fourth"] {
                crate::compositor::tab_binding::open_hosted_app(
                    &mut tree,
                    AppKind::Terminal,
                    title,
                    crate::shell::tab_binding::Placement::NewGroup,
                );
            }
            let strip_id = tree.desktop.workspaces[0].strips[0].id;
            let ids = tree.desktop.workspaces[0].strips[0]
                .window_groups
                .iter()
                .map(|group| group.id)
                .collect::<Vec<_>>();
            tree.desktop.set_active_group(strip_id, ids[0]);

            let chrome = ChromeState::new(tree);
            {
                let mut state = chrome.borrow_mut();
                state.hosted_inputs =
                    HostedTileInputs::for_host_directory_privately(directory.path()).unwrap();
                state.set_strip_viewport(Some(tilo_desktop_model::commands::StripViewport {
                    strip: strip_id,
                    groups: ids
                        .iter()
                        .enumerate()
                        .map(|(index, id)| {
                            let index =
                                f64::from(u32::try_from(index).expect("fixture group index"));
                            tilo_desktop_model::commands::GroupSpan {
                                id: *id,
                                x0: index * 100.0,
                                x1: index.mul_add(100.0, 100.0),
                            }
                        })
                        .collect(),
                    visible_x0: 0.0,
                    visible_x1: 350.0,
                }));
            }

            let mut doc = automerge::AutoCommit::new();
            let results = NativeApplicationsProvider::default().query(SearchContext {
                query_scope: None,
                include_programmatic: false,
                needle: "Files",
                wanted: &[ResourceKind::Application],
                chrome: &chrome.borrow(),
                doc: &doc,
                doc_root: &automerge::ROOT,
            });
            (results[0].on_open)(
                &mut ui::widget::Edits::lend(&mut doc),
                &automerge::ROOT,
                &chrome,
                &ActionArgs::none(),
            )
            .unwrap();

            let state = chrome.borrow();
            let tree = state.tree();
            let groups = &tree.desktop.active_workspace().unwrap().strips[0].window_groups;
            let files_group = groups
                .iter()
                .find(|group| !ids.contains(&group.id))
                .expect("Files search result created a group");
            assert!(
                files_group.tabs.iter().any(|tab| {
                    matches!(
                        &tab.tile,
                        crate::shell::state::TileContent::AppPlaceholder {
                            kind: AppKind::FileExplorer,
                            ..
                        }
                    )
                }),
                "the new group contains the real hosted Files app",
            );
            let files_group = files_group.id;
            assert_eq!(
                groups.iter().map(|group| group.id).collect::<Vec<_>>(),
                vec![ids[0], files_group, ids[1], ids[2], ids[3]],
                "the native search provider opens beside the focused window, like every other search result",
            );
            drop(tree);
            drop(state);
        }
    }
}

#[allow(
    unused_imports,
    reason = "The native WorkspaceExt methods are selected by host-specific workspace code and test configurations."
)]
use crate::shell::state::WorkspaceExt as _;
