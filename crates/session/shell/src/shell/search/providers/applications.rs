//! Applications provider: apps you can open, launchable from the palette.
//!
//! Typing an installed app's name into Ctrl+K offers `Open <App>`, launched
//! through [`crate::shell::launcher`]. Registry apps that are not installed
//! intentionally take a different, safe route: the palette opens the Store's
//! stable detail URI. The Store then shows provenance and offers the current
//! revalidated install action; palette activation never downloads software.

use crate::shell::state::WorkspaceAuthority as _;
use std::cell::Ref;
use std::collections::HashSet;
use std::rc::Rc;
#[cfg(test)]
use std::sync::{Arc, Mutex};

use super::background_index::BackgroundIndex;
use crate::portal::desktop_services::DynamicLauncherRecord;
use crate::shell::catalogue::{self, CatalogueApp};
use crate::shell::chrome_state::{Chrome, OpenTarget};
use crate::shell::i18n::{t, t_args};
use crate::shell::launcher::{self, InstalledApp};
use crate::shell::search::action_api::{ActionArgs, ActionError};
use crate::shell::search::provider_score;
use crate::shell::search::{
    ResourceKind, SearchBudget, SearchContext, SearchCursor, SearchPage, SearchProvider,
    SearchResult,
};
use tilo_desktop::notifications::{Notification, NotificationSeverity};
use tilo_search::{Document, Matching, Page, SearchIndex};

/// Chip label / group header for launchable apps; also the provider's `name()`.
pub const APPLICATIONS_KIND: &str = "Applications";

/// Penalty for an app the machine does not have yet, against one it does —
/// enough to break a tie without dragging a better match below a worse one
/// (every tier of [`application_fields`] is at least five points apart).
const NOT_INSTALLED_TIEBREAK: i32 = 1;
const STORE_PACKAGE_ID: &str = "so.tilo.store";

fn presentation_score(needle: &str, package_id: &str, match_score: i32) -> i32 {
    if needle.is_empty() {
        ui::search::system::starting_point_rank(package_id)
    } else {
        match_score
    }
}

/// The empty palette's second line: the app's starting-point group, then
/// its metadata.
fn grouped_detail(package_id: &str, metadata: String) -> String {
    t_args(
        "search-apps-detail-grouped",
        &[
            (
                "group",
                ui::search::system::starting_point_group(package_id).into(),
            ),
            ("detail", metadata.into()),
        ],
    )
}

fn app_detail(needle: &str, package_id: &str, category: Option<&str>, version: &str) -> String {
    let metadata = match category {
        Some(category) => format!("Tilo · {category} · {package_id}"),
        None => format!("Tilo · {package_id} · {version}"),
    };
    if needle.is_empty() {
        grouped_detail(package_id, metadata)
    } else {
        metadata
    }
}

pub struct ApplicationsProvider {
    kinds: Vec<ResourceKind>,
    index: BackgroundIndex<(u64, u64, u64, u128), ApplicationsSnapshot>,
    #[cfg(test)]
    source: Option<Arc<ApplicationsTestSource>>,
}

#[cfg(test)]
type ApplicationsTestState = (
    Vec<InstalledApp>,
    Vec<CatalogueApp>,
    Vec<crate::shell::host_apps::HostApp>,
    Vec<DynamicLauncherRecord>,
);

#[cfg(test)]
struct ApplicationsTestSource {
    generation: std::sync::atomic::AtomicU64,
    state: Mutex<ApplicationsTestState>,
    materializations: std::sync::atomic::AtomicUsize,
}

#[cfg(test)]
impl ApplicationsTestSource {
    fn new(
        installed: Vec<InstalledApp>,
        catalogue: Vec<CatalogueApp>,
        host_apps: Vec<crate::shell::host_apps::HostApp>,
        dynamic: Vec<DynamicLauncherRecord>,
    ) -> Arc<Self> {
        Arc::new(Self {
            generation: std::sync::atomic::AtomicU64::new(1),
            state: Mutex::new((installed, catalogue, host_apps, dynamic)),
            materializations: std::sync::atomic::AtomicUsize::new(0),
        })
    }

    fn generation(&self) -> (u64, u64, u64, u128) {
        let generation = self.generation.load(std::sync::atomic::Ordering::Acquire);
        (generation, generation, generation, u128::from(generation))
    }

    fn snapshot(
        &self,
    ) -> (
        u64,
        Vec<InstalledApp>,
        Vec<CatalogueApp>,
        Vec<crate::shell::host_apps::HostApp>,
        Vec<DynamicLauncherRecord>,
    ) {
        self.materializations
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let generation = self.generation.load(std::sync::atomic::Ordering::Acquire);
        let (installed, catalogue, host_apps, dynamic) = self
            .state
            .lock()
            .expect("application test source lock")
            .clone();
        (generation, installed, catalogue, host_apps, dynamic)
    }

    fn replace_catalogue(&self, catalogue: Vec<CatalogueApp>) {
        self.state.lock().expect("application test source lock").1 = catalogue;
        self.generation
            .fetch_add(1, std::sync::atomic::Ordering::Release);
    }
}

enum ApplicationEntry {
    Installed(InstalledApp),
    Dynamic(DynamicLauncherRecord),
    Catalogue(CatalogueApp),
}

struct ApplicationsSnapshot {
    entries: Vec<ApplicationEntry>,
    /// Canonical lower-cased identities make the warm catalogue membership
    /// check O(1).  Re-walking either complete source for every indexed row
    /// turns a broad palette page into a quadratic catalogue query.
    installed_ids: HashSet<String>,
    host_app_ids: HashSet<String>,
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
    fn with_test_source(source: Arc<ApplicationsTestSource>) -> Self {
        Self {
            source: Some(source),
            ..Self::new()
        }
    }

    #[allow(clippy::unused_self, reason = "test builds use the injected source")]
    fn source_generation(&self) -> (u64, u64, u64, u128) {
        #[cfg(test)]
        if let Some(source) = &self.source {
            return source.generation();
        }
        (
            launcher::gui_apps_generation(),
            catalogue::available_apps_generation(),
            crate::shell::host_apps::host_apps_generation(),
            crate::portal::desktop_services::dynamic_launcher_generation(),
        )
    }

    /// Refresh the retained projection during the registry's source-refresh
    /// pass.  `query_page` deliberately never calls this: a source change may
    /// contain a very large installed/catalogue corpus and must not turn the
    /// first keystroke into an indexing job.
    fn build_snapshot(
        #[cfg(test)] source: Option<Arc<ApplicationsTestSource>>,
    ) -> ((u64, u64, u64, u128), ApplicationsSnapshot) {
        #[cfg(test)]
        if let Some(source) = source {
            let (generation, installed, catalogue_apps, host_apps, dynamic) = source.snapshot();
            return (
                (generation, generation, generation, u128::from(generation)),
                Self::snapshot_from_parts(&installed, catalogue_apps, &host_apps, dynamic),
            );
        }
        let (installed_generation, installed) = launcher::gui_apps_snapshot();
        let (catalogue_generation, catalogue_apps) = catalogue::available_apps_snapshot();
        let (host_generation, host_apps) = crate::shell::host_apps::host_apps_snapshot();
        let (dynamic_generation, dynamic) =
            crate::portal::desktop_services::dynamic_launcher_snapshot();
        (
            (
                installed_generation,
                catalogue_generation,
                host_generation,
                dynamic_generation,
            ),
            Self::snapshot_from_parts(&installed, catalogue_apps, &host_apps, dynamic),
        )
    }

    fn snapshot_from_parts(
        installed: &[InstalledApp],
        catalogue_apps: Vec<CatalogueApp>,
        host_apps: &[crate::shell::host_apps::HostApp],
        dynamic: Vec<DynamicLauncherRecord>,
    ) -> ApplicationsSnapshot {
        let installed_ids: HashSet<String> = installed
            .iter()
            .map(|app| app.package_id.to_ascii_lowercase())
            .collect();
        let host_app_ids: HashSet<String> = host_apps
            .iter()
            .map(|host| host.desktop_id.to_ascii_lowercase())
            .collect();
        let mut seen_catalogue = HashSet::new();
        let mut entries = installed
            .iter()
            .cloned()
            .map(ApplicationEntry::Installed)
            .collect::<Vec<_>>();
        entries.extend(dynamic.into_iter().map(ApplicationEntry::Dynamic));
        entries.extend(
            catalogue_apps
                .into_iter()
                .filter(|app| {
                    let package_id = app.package_id.to_ascii_lowercase();
                    !installed_ids.contains(&package_id)
                        && !host_app_ids.contains(&package_id)
                        && seen_catalogue.insert(package_id)
                })
                .map(ApplicationEntry::Catalogue),
        );
        let index = index_entries(&entries);
        ApplicationsSnapshot {
            entries,
            installed_ids,
            host_app_ids,
            index,
        }
    }

    /// The worker reads and clones every source itself; the UI tick never
    /// flattens an application corpus before spawn, and a query never starts it.
    #[allow(clippy::unused_self, reason = "test builds use the injected source")]
    fn builder(
        &self,
    ) -> impl FnOnce() -> ((u64, u64, u64, u128), ApplicationsSnapshot) + Send + 'static {
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
    pub(crate) fn results_for(
        &self,
        needle: &str,
        apps: Vec<InstalledApp>,
        open_target: OpenTarget,
    ) -> Vec<SearchResult> {
        let scores = test_scores(needle, apps.iter().map(installed_fields));
        self.installed_rows(needle, apps, &scores, open_target)
    }

    /// Launch rows for the `apps` with a score in `scores`. `open_target`
    /// only words the row — the launch reads the live target again at
    /// activation time, since it can change between the query that built
    /// this row and the Enter that fires it.
    fn installed_rows(
        &self,
        needle: &str,
        apps: Vec<InstalledApp>,
        scores: &[Option<i32>],
        open_target: OpenTarget,
    ) -> Vec<SearchResult> {
        let kind = ResourceKind::Application;
        let mut out = Vec::new();
        for (app, score) in apps.into_iter().zip(scores) {
            let Some(score) = *score else {
                continue;
            };
            let launch_app = app.clone();
            // A declared category is a more useful second line than the version.
            let detail = app_detail(
                needle,
                &app.package_id,
                app.category.as_deref(),
                &app.version,
            );
            // Where the window will land leads the line, since it's the
            // part of a launch row that changes with context.
            let subtitle = t_args(
                "search-apps-open-subtitle",
                &[
                    ("target", open_target.result_hint().into()),
                    ("detail", detail.into()),
                ],
            );
            out.push(SearchResult::nullary_fallible(
                kind.clone(),
                self.id(),
                app.package_id.clone(),
                t_args(
                    "search-apps-open",
                    &[(
                        "app",
                        app.name_for(&crate::shell::i18n::current_locale()).into(),
                    )],
                ),
                Some(subtitle),
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
        open_target: OpenTarget,
    ) -> Vec<SearchResult> {
        let installed_gui = installed
            .iter()
            .map(|app| app.package_id.to_ascii_lowercase())
            .collect();
        let scores = test_scores(
            needle,
            records
                .iter()
                .map(|record| application_fields(&record.name, &record.id, None, None)),
        );
        self.dynamic_results_for_membership(records, &scores, &installed_gui, open_target)
    }

    fn dynamic_results_for_membership(
        &self,
        records: Vec<DynamicLauncherRecord>,
        scores: &[Option<i32>],
        installed_gui: &HashSet<String>,
        open_target: OpenTarget,
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
                    t_args("search-apps-open", &[("app", record.name.as_str().into())]),
                    Some(t_args(
                        "search-apps-shortcut-subtitle",
                        &[("target", open_target.result_hint().into())],
                    )),
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

    /// Score not-yet-installed apps and build Store-detail rows. `installed`
    /// keeps an app from being offered twice between an install finishing and the
    /// catalogue refetching.
    #[cfg(test)]
    pub(crate) fn catalogue_results_for(
        &self,
        needle: &str,
        apps: Vec<CatalogueApp>,
        installed: &[String],
        open_target: OpenTarget,
    ) -> Vec<SearchResult> {
        self.catalogue_results_for_with_hosts(needle, apps, installed, &[], open_target)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "catalogue-result construction keeps ranking, alias suppression, install state, and action metadata together so duplicate offerings cannot diverge"
    )]
    #[cfg(test)]
    fn catalogue_results_for_with_hosts(
        &self,
        needle: &str,
        apps: Vec<CatalogueApp>,
        installed: &[String],
        host_apps: &[crate::shell::host_apps::HostApp],
        open_target: OpenTarget,
    ) -> Vec<SearchResult> {
        let installed_ids = installed.iter().map(|id| id.to_ascii_lowercase()).collect();
        let host_app_ids = host_apps
            .iter()
            .map(|app| app.desktop_id.to_ascii_lowercase())
            .collect();
        let scores = test_scores(
            needle,
            apps.iter().map(|app| {
                application_fields(
                    &app.display_name,
                    &app.package_id,
                    app.category.as_deref(),
                    Some(&app.description),
                )
            }),
        );
        self.catalogue_results_for_membership(
            needle,
            apps,
            &scores,
            &installed_ids,
            &host_app_ids,
            open_target,
        )
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the callback captures one catalogue row's launch safety and failure behaviour, keeping Store activation identical across result sources"
    )]
    fn catalogue_results_for_membership(
        &self,
        needle: &str,
        apps: Vec<CatalogueApp>,
        scores: &[Option<i32>],
        installed_ids: &HashSet<String>,
        host_app_ids: &HashSet<String>,
        _open_target: OpenTarget,
    ) -> Vec<SearchResult> {
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        for (app, score) in apps.into_iter().zip(scores) {
            let package_id_key = app.package_id.to_ascii_lowercase();
            if installed_ids.contains(&package_id_key)
                || host_app_ids.contains(&package_id_key)
                || !seen.insert(package_id_key)
            {
                continue;
            }
            let Some(score) = *score else {
                continue;
            };
            let package_id = app.package_id.clone();
            let display_name = app.display_name.clone();
            let detail = app_detail(
                needle,
                &app.package_id,
                app.category.as_deref(),
                &app.version,
            );
            let trust = if app.trust.trim().is_empty() {
                t("search-apps-verification-pending")
            } else {
                app.trust.clone()
            };
            let subtitle = t_args(
                "search-apps-store-subtitle",
                &[("trust", trust.into()), ("detail", detail.into())],
            );
            out.push(SearchResult {
                kind: ResourceKind::Application,
                operation_kind: tilo_actions::OperationKind::Effect,
                provider_id: self.id().to_string(),
                id: format!("app:{}", app.package_id),
                title: t_args(
                    "search-apps-view-in-store",
                    &[("app", app.display_name.as_str().into())],
                ),
                subtitle: Some(subtitle),
                badge: None,
                score: presentation_score(needle, &app.package_id, score)
                    .saturating_sub(NOT_INSTALLED_TIEBREAK),
                on_open: Rc::new(move |_doc, _root, chrome: &Chrome, _args: &ActionArgs| {
                    // A palette can be opened and activated repeatedly while
                    // catalogue installation is still running. Keep one
                    // in-flight operation and one acknowledgement; replacing
                    // the pending id would strand the first card forever.
                    if catalogue::pending_notice(&package_id).is_some() {
                        return Ok(());
                    }
                    // Captured now, not re-read at launch: the window may be
                    // a minute away and the user's selection can change.
                    let context = {
                        let st = chrome.borrow();
                        let tree = st.tree();
                        crate::shell::placement::note_placement_for_launch_targeted(
                            &package_id,
                            &tree,
                            st.strip_viewport(),
                            st.open_target,
                        );
                        tree.desktop
                            .active_workspace()
                            .map(|ws| (ws.room, ws.capabilities.clone()))
                    };
                    let Some((room, capabilities)) = context else {
                        // No active workspace is no room to launch into —
                        // reported exactly as an installed app's launch would.
                        let mut st = chrome.borrow_mut();
                        st.push_notification_full(
                            Notification::new(
                                t_args(
                                    "search-apps-store-failed",
                                    &[("app", display_name.as_str().into())],
                                ),
                                launcher::LaunchError::NoWorkspace.notification_message(),
                            )
                            .with_severity(NotificationSeverity::Error)
                            .with_app_name(t("host-source-applications"))
                            .with_dismiss_after(launcher::LAUNCH_FAILURE_NOTICE_LIFETIME),
                        );
                        st.bump();
                        // The notification is how the person at the machine
                        // learns; this is how a caller with no screen learns —
                        // the same failure, rather than success reported for an
                        // app that was never started.
                        return Err(ActionError::Refused(format!(
                            "Couldn't open App Store for {display_name}: {}",
                            launcher::LaunchError::NoWorkspace.user_message()
                        )));
                    };
                    let uri = store_detail_uri(&package_id)?;
                    launcher::launch_uri_in_room(STORE_PACKAGE_ID, room, capabilities, &uri)
                        .map(|_| ())
                        .map_err(|error| {
                            let message = error.user_message();
                            let mut state = chrome.borrow_mut();
                            state.push_notification_full(
                                Notification::new(
                                    t_args(
                                        "search-apps-store-failed",
                                        &[("app", display_name.as_str().into())],
                                    ),
                                    message.clone(),
                                )
                                .with_severity(NotificationSeverity::Error)
                                .with_app_name(t("host-source-applications")),
                            );
                            state.bump();
                            ActionError::Refused(format!(
                                "Couldn't open App Store for {display_name}: {message}"
                            ))
                        })
                }),
                on_invoke: None,
                on_begin_invoke: None,
                params: Vec::new(),
                contract: None,
            });
        }
        out
    }
}

fn store_detail_uri(package_id: &str) -> Result<String, ActionError> {
    if package_id.is_empty()
        || package_id.len() > 255
        || package_id
            .chars()
            .any(|ch| !ch.is_ascii_alphanumeric() && ch != '.' && ch != '-')
    {
        return Err(ActionError::Refused(
            "The catalogue entry has an invalid stable package identity".into(),
        ));
    }
    Ok(format!("tilo-store:app/{package_id}"))
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
/// but the workspace projection writes `ws.capabilities` wholesale
/// (`state.rs`), so the grant still reaches every peer on the next persist.
pub(crate) fn grant_file_manager_home(
    tree: &mut crate::shell::state::WorkspaceTree,
    workspace: crate::shell::state::WorkspaceId,
    package_id: &str,
) -> bool {
    use crate::shell::state::{Capability, FsMode, GRANT_SESSION, ResourceRef, Verb};

    if package_id != SYSTEM_FILE_MANAGER {
        return false;
    }
    let Some(home) = tilo_paths::home() else {
        return false;
    };
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
        &ResourceRef::FsPath(home.to_string_lossy().into_owned()),
        Verb::Read,
    ) {
        return false;
    }
    let ids = ws.grant_app(
        package_id,
        &Capability::Filesystem {
            path: home,
            mode: FsMode::ReadWrite,
            scope: GRANT_SESSION,
        },
    );
    let remembered = tree.native_runtime.remember_files_home_consent(ws, &ids);
    if !remembered && tree.native_authority.requires(ws.room) {
        // Native session consent must have exact live local provenance. A
        // missing identity or full ledger must not leave an unbound home row.
        ws.capabilities
            .retain(|capability| !ids.contains(&capability.id));
        return false;
    }
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
    // worker and opening card. Check that transaction before consulting the
    // registry: the package can disappear while its queued worker is still
    // resolving, but a repeated click must not turn that existing launch into
    // an unrelated "not installed" error.
    if catalogue::pending_notice(package_id).is_some() {
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

/// Surface a launch that failed before spawn: retire its opening card, post
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
        // Claimed back rather than left to expire, or the next window
        // to paint would retire somebody else's notice.
        if let Some(id) = catalogue::take_pending_notice(package_id) {
            st.dismiss_notification(id);
        }
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
        state.focus_tab(workspace, strip, group, tab);
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
    // a second copy. See `peer_windows::opened_for_the_room`.
    let already = {
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
        chrome.borrow_mut().focus_tab(ws_id, strip, group, tab);
        return Ok(());
    }
    // The app is already being installed/launched. Repeated activation should
    // be idempotent: one worker, one pending notice, one eventual window.
    if catalogue::pending_notice(package_id).is_some() {
        return Ok(());
    }
    // Say the app is opening before the work starts, so the
    // acknowledgement lands on the same frame as the keystroke. Retired by
    // the window appearing (`retire_notices_for_opened_windows`), not a timer.
    let notice = {
        let mut st = chrome.borrow_mut();
        let id = st.push_notification_full(
            Notification::new(
                t_args("search-apps-opening", &[("app", display_name.into())]),
                t("search-apps-opening-body"),
            )
            .with_app_name(t("host-source-applications"))
            .with_dismiss_after(std::time::Duration::from_secs(120)),
        );
        st.bump();
        id
    };
    catalogue::note_pending_notice(package_id, notice);
    // Scoped so the borrow is released before the error branch below
    // takes a mutable one.
    {
        let st = chrome.borrow();
        let tree = st.tree();
        // Decided here, while selection and layout are still true — the
        // window itself maps seconds later, on another process.
        crate::shell::placement::note_placement_for_launch_targeted(
            package_id,
            &tree,
            st.strip_viewport(),
            st.open_target,
        );
    }
    // The person opening the app is its author. The queued launch copies
    // the capability list now, so the grant has to be in that copy.
    let launched = {
        let st = chrome.borrow();
        let mut tree = st.tree_mut();
        let native_room = tree
            .desktop
            .active_workspace()
            .map(|workspace| workspace.room)
            .filter(|room| tree.requires_native_owner(*room));
        if let Some(room) = native_room {
            drop(tree);
            Some(launcher::queue_native_authoring_launch(
                app.clone(),
                st.tree_handle(),
                room,
                st.native_app_opening.clone(),
            ))
        } else {
            tree.workspace_after_opening_app(package_id)
                .map(|workspace| launcher::queue_launch_in_workspace(app.clone(), &workspace))
        }
    };
    // Non-blocking past resolution — only pre-fork failures land here, the
    // rest arrive as launch events drained each frame.
    let outcome = launched.unwrap_or(Err(launcher::LaunchError::NoWorkspace));
    // How a caller that cannot see the screen learns the launch failed.
    outcome.map_err(|err| report_refused_launch(chrome, package_id, display_name, &err))
}

impl Default for ApplicationsProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl SearchProvider for ApplicationsProvider {
    fn id(&self) -> &'static str {
        "applications"
    }
    fn name(&self) -> &str {
        APPLICATIONS_KIND
    }
    fn resource_kinds(&self) -> &[ResourceKind] {
        &self.kinds
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

    fn refresh_search_index(&self, _ctx: &SearchContext<'_>) {
        self.index
            .refresh(&self.source_generation(), self.builder());
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
        let offset = match cursor {
            Some(cursor) if cursor.revision != revision => return SearchPage::stale(revision),
            Some(cursor) => match cursor
                .token
                .strip_prefix("applications:")
                .and_then(|token| {
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
        let mut considered = 0usize;
        let mut bytes = 0usize;
        let mut results = Vec::new();
        for hit in &found.hits {
            let Some(entry) = usize::try_from(hit.key)
                .ok()
                .and_then(|at| snapshot.entries.get(at))
            else {
                considered = considered.saturating_add(1);
                continue;
            };
            let score = provider_score(hit.rank.unwrap_or(0));
            let Some(row) = self.entry_row(&ctx, &snapshot, entry, score) else {
                considered = considered.saturating_add(1);
                continue;
            };
            let row_bytes = row
                .title
                .len()
                .saturating_add(row.subtitle.as_ref().map_or(0, String::len));
            if bytes.saturating_add(row_bytes) > budget.max_bytes {
                break;
            }
            bytes = bytes.saturating_add(row_bytes);
            considered = considered.saturating_add(1);
            results.push(row);
        }
        let exhausted = !found.more && considered == found.hits.len();
        let next = offset.saturating_add(considered);
        SearchPage {
            revision: revision.clone(),
            results,
            next_cursor: (!exhausted).then(|| SearchCursor {
                revision,
                token: format!("applications:{next}\u{1f}{}", ctx.needle),
            }),
            exhausted,
            stale_cursor: false,
            budget_exhausted: !exhausted,
        }
    }
}

impl ApplicationEntry {
    /// Whether the row this entry contributes is addressed by `id`, decided
    /// without building the row.
    fn is_addressed_by(&self, id: &str) -> bool {
        match self {
            Self::Installed(app) => app.package_id == id,
            Self::Dynamic(record) => record.id == id,
            Self::Catalogue(app) => id
                .strip_prefix("app:")
                .is_some_and(|package_id| package_id == app.package_id),
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
            ApplicationEntry::Installed(app) => self.installed_rows(
                ctx.needle,
                vec![app.clone()],
                &score,
                ctx.chrome.open_target,
            ),
            ApplicationEntry::Dynamic(record) => self.dynamic_results_for_membership(
                vec![record.clone()],
                &score,
                &snapshot.installed_ids,
                ctx.chrome.open_target,
            ),
            ApplicationEntry::Catalogue(app) => self.catalogue_results_for_membership(
                ctx.needle,
                vec![app.clone()],
                &score,
                &snapshot.installed_ids,
                &snapshot.host_app_ids,
                ctx.chrome.open_target,
            ),
        };
        rows.pop()
    }

    fn revision(&self) -> String {
        let (installed, catalogue, host, dynamic) = self.source_generation();
        let ready = self.index.is_ready(&(installed, catalogue, host, dynamic));
        format!(
            "{}:{installed}:{catalogue}:{host}:{dynamic}",
            if ready { "ready" } else { "indexing" }
        )
    }
}

pub(crate) use ui::search::system::{PACKAGE_ID_WEIGHT, application_fields};

/// Applications match words spread over their fields, and typos in names.
const APPLICATION_MATCHING: Matching = Matching::LOOSE;

/// Each app's provider score for `needle`, for tests building rows from a
/// bare list.
#[cfg(test)]
fn test_scores<'a, Fields>(needle: &str, apps: impl Iterator<Item = Fields>) -> Vec<Option<i32>>
where
    Fields: IntoIterator<Item = (&'a str, tilo_search::FieldWeight)>,
{
    ui::search::ListIndex::new(APPLICATION_MATCHING)
        .ranks(needle, apps)
        .into_iter()
        .map(|rank| rank.map(provider_score))
        .collect()
}

/// An installed app's [`application_fields`], plus its name in every
/// language its manifest gives one, each weighed as the name itself: a
/// German desktop types "Dateien", an English habit types "Files".
fn installed_fields(app: &InstalledApp) -> Vec<(&str, tilo_search::FieldWeight)> {
    let mut fields = application_fields(
        &app.display_name,
        &app.package_id,
        app.category.as_deref(),
        None,
    )
    .to_vec();
    fields.extend(
        app.localized_names
            .values()
            .map(|name| (name.as_str(), tilo_search::FieldWeight::TITLE)),
    );
    fields
}

/// Index each entry's [`application_fields`], keyed by position.
fn index_entries(entries: &[ApplicationEntry]) -> SearchIndex {
    fn app_fields(
        document: Document,
        name: &str,
        package_id: &str,
        category: Option<&str>,
        description: Option<&str>,
    ) -> Document {
        application_fields(name, package_id, category, description)
            .into_iter()
            .fold(document, |document, (text, weight)| {
                document.field(text, weight)
            })
    }
    let mut index = SearchIndex::new(APPLICATION_MATCHING);
    index.replace_all(entries.iter().enumerate().map(|(at, entry)| {
        let document = Document::new(at as u64);
        match entry {
            ApplicationEntry::Installed(app) => installed_fields(app)
                .into_iter()
                .fold(document, |document, (text, weight)| {
                    document.field(text, weight)
                }),
            ApplicationEntry::Dynamic(record) => {
                app_fields(document, &record.name, &record.id, None, None)
            }
            ApplicationEntry::Catalogue(app) => app_fields(
                document,
                &app.display_name,
                &app.package_id,
                app.category.as_deref(),
                Some(&app.description),
            ),
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

    /// Bounds a wait that ends as soon as its condition holds; only a hang reaches it.
    const HANG_BOUND: std::time::Duration = std::time::Duration::from_secs(30);

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
            }],
            active_tab: tab_id,
            width: WindowWidth::Normal,
            maximized: false,
        });
        workspace.strips.push(strip);
        let mut tree = WorkspaceTree::default();
        tree.set_salt_override(crate::shell::state::peer_salt_of(tab_id));
        tree.desktop.active = workspace.id;
        tree.desktop.workspaces = vec![workspace];
        let handle = crate::shell::state::WorkspaceTreeHandle::new(tree);
        let chrome = ChromeState::new_with_handle(handle.clone());
        let provider = ApplicationsProvider::new();
        let rows = provider.results_for(
            "terminal",
            vec![app("so.tilo.terminal", "Terminal")],
            OpenTarget::Beside,
        );
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
            }],
            active_tab: tab,
            width: WindowWidth::Normal,
            maximized: false,
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
        let rows = ApplicationsProvider::new().results_for(
            "browser",
            vec![app("so.tilo.browser", "Browser")],
            OpenTarget::Beside,
        );
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
        let deadline = std::time::Instant::now()
            .checked_add(HANG_BOUND)
            .expect("the hang bound fits in an Instant");
        while std::time::Instant::now() < deadline {
            provider.refresh_search_index(ctx);
            if provider.search_revision(ctx).starts_with("ready:") {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("application search index did not become ready");
    }

    fn app(package_id: &str, display_name: &str) -> InstalledApp {
        app_in(package_id, display_name, None)
    }

    fn offered(package_id: &str, display_name: &str, category: Option<&str>) -> CatalogueApp {
        CatalogueApp {
            package_id: package_id.to_string(),
            display_name: display_name.to_string(),
            description: "does a thing".to_string(),
            category: category.map(str::to_string),
            version: "0.1.0".to_string(),
            registry: "Tilo main".to_string(),
            trust: "verified".to_string(),
        }
    }

    fn app_in(package_id: &str, display_name: &str, category: Option<&str>) -> InstalledApp {
        InstalledApp {
            package_id: package_id.to_string(),
            display_name: display_name.to_string(),
            localized_names: BTreeMap::default(),
            category: category.map(str::to_string),
            version: "0.1.0".to_string(),
            manifest_path: PathBuf::from("/apps/x/manifest.toml"),
            payload_root: PathBuf::from("/apps/x"),
            has_gui: true,
            opens_extensions: Vec::new(),
            opens_mime_types: Vec::new(),
            opens_uri_schemes: Vec::new(),
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn activating_an_app_is_idempotent_and_reports_failures() {
        // Phase `repeated_activation_while_opening_is_idempotent`.
        {
            let chrome = chrome_rc();
            let package_id = "so.tilo.repeated-open-fixture";
            let notice = chrome
                .borrow_mut()
                .push_notification("Opening Fixture", "Loading…");
            catalogue::note_pending_notice(package_id, notice);

            assert!(launch_installed_app(&chrome, package_id, "Fixture").is_ok());
            assert_eq!(
                chrome.borrow().notifications().len(),
                1,
                "a second activation must not create another worker or opening card"
            );
            assert_eq!(catalogue::take_pending_notice(package_id), Some(notice));
            chrome.borrow_mut().dismiss_notification(notice);
        }

        // Phase `activating_with_no_workspace_reports_it_the_same_way`.
        // Must say so in the same words an installed app's launch uses, not
        // fail silently. Drives the real `on_open` closure.
        {
            let p = ApplicationsProvider::new();
            let c = chrome_rc();
            let mut doc = AutoCommit::new();
            let out = p.catalogue_results_for(
                "sheets",
                vec![offered("so.tilo.sheets", "Sheets", None)],
                &[],
                OpenTarget::Beside,
            );
            let row = out.first().expect("a row for the offered app");
            assert!(c.borrow().notifications().is_empty());
            let opened = (row.on_open)(
                &mut ui::widget::Edits::lend(&mut doc),
                &automerge::ROOT,
                &c,
                &ActionArgs::none(),
            );
            assert!(
                opened.is_err(),
                "no active workspace is no room to launch into — the caller must be told, \
                 not answered as though the app had started",
            );
            let st = c.borrow();
            let n = st
                .notifications()
                .last()
                .expect("activation must never be silent");
            assert_eq!(n.title, "Couldn't open App Store for Sheets");
            assert_eq!(n.severity, NotificationSeverity::Error);
            assert_eq!(
                n.body,
                launcher::LaunchError::NoWorkspace.notification_message(),
                "the same failure gets the same words whichever list the app \
                 came from"
            );
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
            let out = p.results_for(
                "ghost",
                vec![app("so.tilo.ghost", "Ghost")],
                OpenTarget::Beside,
            );
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
            let out = p.results_for(
                "hel",
                vec![app("so.tilo.hello", "Hello")],
                OpenTarget::Beside,
            );
            assert_eq!(out.len(), 1);
            assert_eq!(out[0].title, "Open Hello");
            assert_eq!(out[0].id, "so.tilo.hello");
            assert_eq!(out[0].score, 100, "prefix match on the display name");
            assert_eq!(
                out[0].subtitle.as_deref(),
                Some("Opens in rightmost space · Tilo · so.tilo.hello · 0.1.0"),
                "the row names the package it will launch"
            );
        }

        // Phase `available_apps_are_discoverable_by_description_and_cross_field_terms`.
        {
            let p = ApplicationsProvider::new();
            let mut candidate = offered("so.tilo.ledger", "Ledger", Some("Office"));
            candidate.description = "Fast collaborative spreadsheet".to_string();

            let by_description = p.catalogue_results_for(
                "spreadsheet",
                vec![candidate.clone()],
                &[],
                OpenTarget::Beside,
            );
            assert_eq!(
                by_description.first().map(|row| row.id.as_str()),
                Some("app:so.tilo.ledger")
            );

            let across_fields = p.catalogue_results_for(
                "office collaborative",
                vec![candidate],
                &[],
                OpenTarget::Beside,
            );
            assert_eq!(
                across_fields.first().map(|row| row.id.as_str()),
                Some("app:so.tilo.ledger")
            );
        }

        // Phase `declared_category_is_matchable_and_replaces_the_version_subtitle`.
        {
            let p = ApplicationsProvider::new();
            let out = p.results_for(
                "office",
                vec![
                    app_in("so.tilo.sheets", "Sheets", Some("Office")),
                    app_in("so.tilo.calculator", "Calculator", Some("Utility")),
                ],
                OpenTarget::Beside,
            );
            assert_eq!(out.len(), 1, "only the Office app matched: {out:?}");
            assert_eq!(out[0].id, "so.tilo.sheets");
            assert_eq!(out[0].score, 34, "category ranks below name and id matches");
            assert_eq!(
                out[0].subtitle.as_deref(),
                Some("Opens in rightmost space · Tilo · Office · so.tilo.sheets"),
                "a declared category is the more useful second line than the version"
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
                    app_in("so.tilo.monitor", "Monitor", Some("System")),
                    app_in("so.tilo.sysinfo", "System Info", Some("Utility")),
                ],
                OpenTarget::Beside,
            );
            assert_eq!(out.len(), 2);
            let score = |id: &str| out.iter().find(|r| r.id == id).unwrap().score;
            assert_eq!(score("so.tilo.sysinfo"), 100, "prefix on the display name");
            assert_eq!(score("so.tilo.monitor"), 34, "category-only match");
            assert!(score("so.tilo.sysinfo") > score("so.tilo.monitor"));
        }

        // Phase `package_id_matches_score_below_name_matches`.
        {
            let p = ApplicationsProvider::new();
            let out = p.results_for(
                "so.tilo",
                vec![app("so.tilo.hello", "Hello"), app("org.x.zed", "Zed")],
                OpenTarget::Beside,
            );
            assert_eq!(out.len(), 1, "only the matching package: {out:?}");
            // A shared id *prefix* names a publisher, not an app, so it
            // stays below a name match.
            assert_eq!(out[0].score, 93);
            let by_name = p.results_for(
                "hello",
                vec![app("so.tilo.hello", "Hello")],
                OpenTarget::Beside,
            );
            assert!(
                by_name[0].score > out[0].score,
                "naming the app must still beat naming its publisher"
            );
        }

        // Phase `the_id_last_segment_is_an_identity_match`.
        // The display name is "Hex Editor", which never prefix-matches the id.
        {
            let p = ApplicationsProvider::new();
            let out = p.results_for(
                "hex-editor",
                vec![app("so.tilo.hex-editor", "Hex Editor")],
                OpenTarget::Beside,
            );
            assert_eq!(out.len(), 1, "the app must match at all: {out:?}");
            assert_eq!(
                out[0].score, 100,
                "naming an app's id segment identifies it as exactly as \
                 naming the app does"
            );
            let full = p.results_for(
                "so.tilo.hex-editor",
                vec![app("so.tilo.hex-editor", "Hex Editor")],
                OpenTarget::Beside,
            );
            assert_eq!(full[0].score, 100);
        }

        // Phase `non_matching_needle_returns_nothing`.
        {
            let p = ApplicationsProvider::new();
            let out = p.results_for(
                "zzzqqq",
                vec![app("so.tilo.hello", "Hello")],
                OpenTarget::Beside,
            );
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
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn application_rows_dedupe_only_true_identity_and_hide_malformed_records() {
        // Phase `duplicate_registry_offers_produce_one_application_row`.
        {
            let p = ApplicationsProvider::new();
            let app = offered("so.tilo.notes", "Notes", Some("Office"));
            let out =
                p.catalogue_results_for("notes", vec![app.clone(), app], &[], OpenTarget::Beside);
            assert_eq!(out.len(), 1, "registry mirrors must not duplicate an app");
        }

        // Phase `an_exact_host_package_identity_hides_its_catalogue_alias`.
        {
            let p = ApplicationsProvider::new();
            let hosts = vec![crate::shell::host_apps::HostApp {
                desktop_id: "so.tilo.notes".into(),
                name: "Notes".into(),
                source_label: "Flatpak",
                categories: Vec::new(),
                keywords: Vec::new(),
                mime_types: Vec::new(),
                terminal: false,
                window_identities: vec!["so.tilo.notes".into()],
                icon: None,
            }];
            let out = p.catalogue_results_for_with_hosts(
                "notes",
                vec![offered("so.tilo.notes", "Notes", Some("Office"))],
                &[],
                &hosts,
                OpenTarget::Beside,
            );
            assert!(
                out.is_empty(),
                "opening an existing app must not offer a duplicate fetch"
            );
        }

        // Phase `gnome_calendar_does_not_hide_tilo_calendar`.
        {
            let rows = catalogue_rows_with_host(
                "so.tilo.calendar",
                "Calendar",
                "org.gnome.Calendar",
                "Calendar",
            );
            assert_eq!(rows.len(), 1);
            assert!(rows[0].subtitle.as_deref().unwrap().contains("Tilo"));
        }

        // Phase `host_terminal_does_not_hide_tilo_terminal`.
        {
            let rows = catalogue_rows_with_host(
                "so.tilo.terminal",
                "Terminal",
                "org.gnome.Console",
                "Terminal",
            );
            assert_eq!(rows.len(), 1);
        }

        // Phase `a_generic_shared_name_is_not_cross_provider_identity`.
        {
            let rows = catalogue_rows_with_host(
                "so.tilo.notes",
                "Notes",
                "org.example.PrivateNotes",
                "Notes",
            );
            assert_eq!(rows.len(), 1);
        }

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
                OpenTarget::Beside,
            );
            assert_eq!(out.len(), 1);
            assert_eq!(out[0].id, "portal-valid");
            assert_eq!(out[0].title, "Open Project board");
            assert!(out[0].subtitle.as_deref().unwrap().contains("Shortcut"));
        }
    }

    fn catalogue_rows_with_host(
        catalogue_id: &str,
        catalogue_name: &str,
        host_id: &str,
        host_name: &str,
    ) -> Vec<SearchResult> {
        let p = ApplicationsProvider::new();
        let hosts = vec![crate::shell::host_apps::HostApp {
            desktop_id: host_id.into(),
            name: host_name.into(),
            source_label: "System",
            categories: Vec::new(),
            keywords: Vec::new(),
            mime_types: Vec::new(),
            terminal: false,
            icon: None,
            window_identities: vec![host_id.into()],
        }];
        p.catalogue_results_for_with_hosts(
            catalogue_name,
            vec![offered(catalogue_id, catalogue_name, None)],
            &[],
            &hosts,
            OpenTarget::Beside,
        )
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
                OpenTarget::Beside,
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
                    .find(|row| row.id == "so.tilo.browser")
                    .and_then(|row| row.subtitle.as_deref())
                    .is_some_and(|subtitle| subtitle.contains("Start here")),
                "the ordering must also be explained on screen"
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
                OpenTarget::Beside,
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

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn uninstalled_apps_open_their_store_detail_and_never_install() {
        // Phase `an_uninstalled_app_opens_its_canonical_store_detail`.
        {
            let p = ApplicationsProvider::new();
            let out = p.catalogue_results_for(
                "she",
                vec![offered("so.tilo.sheets", "Sheets", Some("Office"))],
                &[],
                OpenTarget::Beside,
            );
            assert_eq!(out.len(), 1, "the offered app must match: {out:?}");
            assert_eq!(
                out[0].title, "View Sheets in App Store",
                "palette activation must present a reviewable Store detail, not install"
            );
            assert_eq!(out[0].id, "app:so.tilo.sheets");
            assert_eq!(out[0].kind, ResourceKind::Application);
            assert!(
                out[0].subtitle.as_deref().is_some_and(
                    |subtitle| subtitle.contains("App Store") && subtitle.contains("verified")
                ),
                "the palette must disclose the review surface and trust claim"
            );
            assert_eq!(
                out[0].provider_id, "applications",
                "one provider means one group header — a second would be a \
                 visible 'other kind of app' section"
            );
        }

        // Phase `palette_result_never_installs_and_store_uri_is_stable`.
        {
            let p = ApplicationsProvider::new();
            let out = p.catalogue_results_for(
                "",
                vec![offered("so.tilo.sheets", "Sheets", Some("Office"))],
                &[],
                OpenTarget::Beside,
            );
            let row = &out[0];
            let visible = format!("{} {}", row.title, row.subtitle.clone().unwrap_or_default());
            let lowered = visible.to_lowercase();
            for word in ["install", "download", "fetch", "available", "get "] {
                assert!(
                    !lowered.contains(word),
                    "{word:?} must not appear in a row the user reads as opening \
                     an app: {visible:?}"
                );
            }
            assert_eq!(
                store_detail_uri("so.tilo.sheets").unwrap(),
                "tilo-store:app/so.tilo.sheets"
            );
            assert!(store_detail_uri("so.tilo.sheets?install=true").is_err());
        }

        // Phase `uninstalled_rows_are_explicit_about_the_store_review_surface`.
        {
            let p = ApplicationsProvider::new();
            let available = p.catalogue_results_for(
                "",
                vec![offered("so.tilo.sheets", "Sheets", Some("Office"))],
                &[],
                OpenTarget::Beside,
            );
            assert!(available[0].title.contains("App Store"));
            assert!(
                available[0]
                    .subtitle
                    .as_deref()
                    .is_some_and(|subtitle| subtitle.contains("verified"))
            );
        }

        // Phase `shell_query_includes_an_uninstalled_app_as_a_store_detail_row`.
        {
            let p = ApplicationsProvider::with_test_source(ApplicationsTestSource::new(
                Vec::new(),
                vec![offered(
                    "so.tilo.uninstalled-sheets",
                    "Sheets",
                    Some("Office"),
                )],
                Vec::new(),
                Vec::new(),
            ));
            let c = chrome_rc();
            let state = c.borrow();
            let doc = AutoCommit::new();
            let ctx = SearchContext {
                query_scope: None,
                include_programmatic: false,
                needle: "sheets",
                wanted: &[ResourceKind::Application],
                chrome: &state,
                doc: &doc,
                doc_root: &automerge::ROOT,
            };
            refresh_until_ready(&p, &ctx);
            let results = p.query(ctx);
            let row = results
                .iter()
                .find(|row| row.id == "app:so.tilo.uninstalled-sheets")
                .expect("Win+K finds the uninstalled stable app identity");
            assert_eq!(row.title, "View Sheets in App Store");
            assert!(
                row.subtitle
                    .as_deref()
                    .is_some_and(|text| text.contains("verified"))
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
            Vec::new(),
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

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn installed_apps_and_catalogue_offers_never_double_up() {
        // Phase `an_app_you_have_wins_a_tie_but_not_a_better_match`.
        {
            let p = ApplicationsProvider::new();
            let mine = p.results_for(
                "notes",
                vec![app("so.tilo.notes", "Notes")],
                OpenTarget::Beside,
            );
            let theirs = p.catalogue_results_for(
                "notes",
                vec![offered("org.other.notes", "Notes", None)],
                &[],
                OpenTarget::Beside,
            );
            assert!(
                mine[0].score > theirs[0].score,
                "at equal match quality the installed app ranks first: \
                 {} vs {}",
                mine[0].score,
                theirs[0].score
            );

            // A weaker match on an installed app must NOT beat the app the
            // user actually named.
            let vague = p.results_for(
                "sheets",
                vec![app("so.tilo.score", "Sheet Music Helper")],
                OpenTarget::Beside,
            );
            let named = p.catalogue_results_for(
                "sheets",
                vec![offered("so.tilo.sheets", "Sheets", None)],
                &[],
                OpenTarget::Beside,
            );
            assert!(
                !vague.is_empty() && !named.is_empty(),
                "both sides must actually match for this to prove anything"
            );
            assert!(
                named[0].score > vague[0].score,
                "naming an app you don't have beats a vague hit on one you do: \
                 {} vs {}",
                named[0].score,
                vague[0].score
            );
        }

        // Phase `an_app_that_just_became_installed_is_not_offered_twice`.
        // The same app can be in both lists briefly; must produce one row, not two.
        {
            let p = ApplicationsProvider::new();
            let out = p.catalogue_results_for(
                "sheets",
                vec![offered("so.tilo.sheets", "Sheets", None)],
                &["so.tilo.sheets".to_string()],
                OpenTarget::Beside,
            );
            assert!(
                out.is_empty(),
                "an installed app must not also be offered as available: {out:?}"
            );
        }

        // Phase `a_non_matching_needle_offers_nothing_from_the_catalogue`.
        {
            let p = ApplicationsProvider::new();
            let out = p.catalogue_results_for(
                "zzzqqq",
                vec![offered("so.tilo.sheets", "Sheets", None)],
                &[],
                OpenTarget::Beside,
            );
            assert!(out.is_empty());
        }

        // Phase `an_app_that_is_already_here_does_not_raise_a_loading_state`.
        // A state that flashes for a hundred milliseconds is noise, not feedback.
        {
            let p = ApplicationsProvider::new();
            let out = p.results_for(
                "she",
                vec![app("so.tilo.sheets", "Sheets")],
                OpenTarget::Beside,
            );
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
            Vec::new(),
            Vec::new(),
        ));
        let entries = (0..=4_000)
            .map(|index| {
                ApplicationEntry::Installed(app_in(
                    &format!("so.tilo.fixture-{index}"),
                    if index == 4_000 {
                        "unique-tail"
                    } else {
                        "common app"
                    },
                    None,
                ))
            })
            .collect::<Vec<_>>();
        let index = index_entries(&entries);
        provider.index.set_ready(
            provider.source_generation(),
            ApplicationsSnapshot {
                entries,
                installed_ids: HashSet::new(),
                host_app_ids: HashSet::new(),
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
        assert_eq!(page.results[0].title, "Open unique-tail");
        assert!(page.exhausted, "the rare posting needs one warm inspection");
        assert!(!provider.index.building());
    }

    #[test]
    fn a_worker_snapshot_is_rejected_when_its_source_revision_changed() {
        let source = ApplicationsTestSource::new(Vec::new(), Vec::new(), Vec::new(), Vec::new());
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
        let deadline = std::time::Instant::now()
            .checked_add(HANG_BOUND)
            .expect("the hang bound fits in an Instant");
        while std::time::Instant::now() < deadline {
            if source
                .materializations
                .load(std::sync::atomic::Ordering::Relaxed)
                == 1
            {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert_eq!(
            source
                .materializations
                .load(std::sync::atomic::Ordering::Relaxed),
            1
        );
        source.replace_catalogue(vec![offered("so.tilo.changed", "Changed", None)]);
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
        let source = ApplicationsTestSource::new(
            installed,
            vec![offered("so.tilo.store-only", "Store Only", None)],
            Vec::new(),
            Vec::new(),
        );
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
            Some(format!("Open Fixture {:03}", count - 1).as_str()),
            "an installed app past the first page must still open by its address"
        );
        assert!(
            select("app:so.tilo.store-only").is_some_and(|title| title.contains("Store Only")),
            "a catalogue offer past the first page must still resolve"
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

        let source = ApplicationsTestSource::new(
            vec![app("so.tilo.calculator", "Calculator")],
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
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
        assert_eq!(opened.as_deref(), Some("Open Calculator"));
    }

    #[test]
    fn source_backed_cold_query_does_no_application_index_work_before_refresh() {
        let source = ApplicationsTestSource::new(
            Vec::new(),
            (0..=4_000)
                .map(|index| CatalogueApp {
                    package_id: format!("so.tilo.cold-fixture-{index}"),
                    display_name: if index == 4_000 {
                        "unique-source-tail".into()
                    } else {
                        "common source app".into()
                    },
                    description: String::new(),
                    category: None,
                    version: "1".into(),
                    registry: "fixture".into(),
                    trust: "verified".into(),
                })
                .collect(),
            Vec::new(),
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
        assert_eq!(
            page.results[0].title,
            "View unique-source-tail in App Store"
        );
    }
}

/// Tiny session-owned catalog of apps the native host can actually construct.
/// It shares application visibility policy and action dispatch with package apps.
#[derive(Default)]
pub struct NativeApplicationsProvider {
    /// The apps' names and ids, indexed while they stay the same.
    ranker: ui::search::ListIndex,
}

impl SearchProvider for NativeApplicationsProvider {
    fn id(&self) -> &'static str {
        "native-applications"
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
        let ranks = self.ranker.ranks(
            ctx.needle,
            apps.iter()
                .map(|app| application_fields(app.display_name, app.package_id, None, None)),
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
                    t_args("search-apps-open", &[("app", app.display_name.into())]),
                    Some(ctx.chrome.open_target.result_hint()),
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
                            OpenTarget::Here(_) => crate::shell::window_actions::Destination::Here,
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

        // Phase `native_app_search_opens_at_the_rightmost_visible_space`.
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
                vec![ids[0], ids[1], ids[2], files_group, ids[3]],
                "the native search provider must use the same rightmost-visible placement as launched apps",
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
