//! Applications provider: apps you can open, launchable from the palette.
//!
//! Typing an installed app's name into Ctrl+K offers `Open <App>`, launched
//! through [`crate::shell::launcher`]. Registry apps that are not installed
//! intentionally take a different, safe route: the palette opens the Store's
//! stable detail URI. The Store then shows provenance and offers the current
//! revalidated install action; palette activation never downloads software.

use std::cell::{Ref, RefCell};
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver, TryRecvError};
#[cfg(test)]
use std::sync::{Arc, Mutex};

use crate::portal::desktop_services::DynamicLauncherRecord;
use crate::shell::catalogue::{self, CatalogueApp};
use crate::shell::chrome_state::{Chrome, OpenTarget};
use crate::shell::launcher::{self, InstalledApp};
use crate::shell::notification::{Notification, NotificationSeverity};
use crate::shell::search::action_api::{ActionArgs, ActionError};
use crate::shell::search::{
    ResourceKind, SearchBudget, SearchContext, SearchCursor, SearchPage, SearchProvider,
    SearchResult,
};

/// Chip label / group header for launchable apps; also the provider's `name()`.
pub const APPLICATIONS_KIND: &str = "Applications";

/// Penalty for an app the machine does not have yet, against one it does —
/// enough to break a tie without dragging a better match below a worse one
/// (every band in [`match_score`] is at least five points apart).
const NOT_INSTALLED_TIEBREAK: i32 = 1;
const STORE_PACKAGE_ID: &str = "so.tilo.store";

/// Product-owned ordering for the empty Applications palette. Search queries
/// continue to rank purely by match quality; this only prevents a fresh system
/// from presenting forty alphabetised tools as though they were equally likely
/// starting points. The first group is deliberately useful to curious
/// tinkerers without assuming they are developers.
fn empty_palette_presentation(package_id: &str) -> (&'static str, i32) {
    match package_id {
        "so.tilo.browser" => ("Start here", 90),
        "so.tilo.file-explorer" => ("Start here", 89),
        "so.tilo.notes" => ("Start here", 88),
        "so.tilo.terminal" => ("Start here", 87),
        "so.tilo.settings" => ("Start here", 86),
        "so.tilo.store" => ("Start here", 85),

        "so.tilo.calendar" => ("Everyday", 74),
        "so.tilo.mail" => ("Everyday", 73),
        "so.tilo.contacts" => ("Everyday", 72),
        "so.tilo.chat" => ("Everyday", 71),
        "so.tilo.calculator" => ("Everyday", 70),

        "so.tilo.word-processor" => ("Work", 59),
        "so.tilo.sheets" => ("Work", 58),
        "so.tilo.slides" => ("Work", 57),
        "so.tilo.pdf-viewer" => ("Work", 56),
        "so.tilo.typst-editor" => ("Work", 55),

        "so.tilo.music" => ("Media", 49),
        "so.tilo.tube" => ("Media", 48),
        "so.tilo.video-player" => ("Media", 47),
        "so.tilo.photos" => ("Media", 46),
        "so.tilo.camera" => ("Media", 45),
        "so.tilo.weather" => ("Everyday", 44),
        "so.tilo.maps" => ("Everyday", 43),
        "so.tilo.feeds" => ("Everyday", 42),
        "so.tilo.ebook" => ("Media", 41),
        "so.tilo.totp" => ("Everyday", 40),

        "so.tilo.monitor" | "so.tilo.disks" | "so.tilo.logs" | "so.tilo.firewall"
        | "so.tilo.vpn" | "so.tilo.remote" | "so.tilo.backups" | "so.tilo.imager"
        | "so.tilo.themes" => ("System tools", 25),
        _ => ("More apps", 10),
    }
}

fn presentation_score(needle: &str, package_id: &str, match_score: i32) -> i32 {
    if needle.is_empty() {
        empty_palette_presentation(package_id).1
    } else {
        match_score
    }
}

fn app_detail(needle: &str, app: &InstalledApp) -> String {
    let metadata = match app.category.as_deref() {
        Some(category) => format!("Tilo · {category} · {}", app.package_id),
        None => format!("Tilo · {} · {}", app.package_id, app.version),
    };
    if needle.is_empty() {
        format!(
            "{} · {metadata}",
            empty_palette_presentation(&app.package_id).0
        )
    } else {
        metadata
    }
}

fn catalogue_app_detail(needle: &str, app: &CatalogueApp) -> String {
    let metadata = match app.category.as_deref() {
        Some(category) => format!("Tilo · {category} · {}", app.package_id),
        None => format!("Tilo · {} · {}", app.package_id, app.version),
    };
    if needle.is_empty() {
        format!(
            "{} · {metadata}",
            empty_palette_presentation(&app.package_id).0
        )
    } else {
        metadata
    }
}

pub struct ApplicationsProvider {
    kinds: Vec<ResourceKind>,
    snapshot: RefCell<Option<ApplicationsSnapshot>>,
    build: RefCell<Option<Receiver<ApplicationsSnapshot>>>,
    #[cfg(test)]
    source: Option<Arc<ApplicationsTestSource>>,
    #[cfg(test)]
    build_probe: Arc<BuildProbe>,
}

#[cfg(test)]
#[derive(Default)]
struct BuildProbe {
    builds: std::sync::atomic::AtomicUsize,
    thread: Mutex<Option<std::thread::ThreadId>>,
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
    source_generation: (u64, u64, u64, u128),
    entries: Vec<ApplicationEntry>,
    /// Canonical lower-cased identities make the warm catalogue membership
    /// check O(1).  Re-walking either complete source for every indexed row
    /// turns a broad palette page into a quadratic catalogue query.
    installed_ids: HashSet<String>,
    host_app_ids: HashSet<String>,
    index: ui::search::SearchTextIndex,
}

impl ApplicationsProvider {
    pub fn new() -> Self {
        Self {
            kinds: vec![ResourceKind::Application],
            snapshot: RefCell::new(None),
            build: RefCell::new(None),
            #[cfg(test)]
            source: None,
            #[cfg(test)]
            build_probe: Arc::new(BuildProbe::default()),
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
        #[cfg(test)] probe: &BuildProbe,
    ) -> ApplicationsSnapshot {
        #[cfg(test)]
        {
            probe
                .builds
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            *probe.thread.lock().expect("application build thread lock") =
                Some(std::thread::current().id());
        }
        #[cfg(test)]
        if let Some(source) = source {
            let (generation, installed, catalogue_apps, host_apps, dynamic) = source.snapshot();
            return Self::snapshot_from_parts(
                (generation, generation, generation, u128::from(generation)),
                &installed,
                catalogue_apps,
                &host_apps,
                dynamic,
            );
        }
        let (installed_generation, installed) = launcher::gui_apps_snapshot();
        let (catalogue_generation, catalogue_apps) = catalogue::available_apps_snapshot();
        let (host_generation, host_apps) = crate::shell::host_apps::host_apps_snapshot();
        let (dynamic_generation, dynamic) =
            crate::portal::desktop_services::dynamic_launcher_snapshot();
        Self::snapshot_from_parts(
            (
                installed_generation,
                catalogue_generation,
                host_generation,
                dynamic_generation,
            ),
            &installed,
            catalogue_apps,
            &host_apps,
            dynamic,
        )
    }

    fn snapshot_from_parts(
        source_generation: (u64, u64, u64, u128),
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
        let index = ui::search::SearchTextIndex::new(entries.iter().map(|entry| match entry {
            ApplicationEntry::Installed(app) => format!(
                "{}\u{1f}{}\u{1f}{}",
                app.display_name,
                app.package_id,
                app.category.as_deref().unwrap_or("")
            ),
            ApplicationEntry::Dynamic(record) => format!(
                "{}\u{1f}{}\u{1f}{}",
                record.name, record.id, record.owner_app_id
            ),
            ApplicationEntry::Catalogue(app) => format!(
                "{}\u{1f}{}\u{1f}{}\u{1f}{}",
                app.display_name,
                app.package_id,
                app.category.as_deref().unwrap_or(""),
                app.description
            ),
        }));
        ApplicationsSnapshot {
            // Never stamp a completed mixed read with a generation fetched
            // afterwards: a source can change while this worker builds.
            // `snapshot()` compares these captured generations to current
            // source generations and rejects a stale worker result.
            source_generation,
            entries,
            installed_ids,
            host_app_ids,
            index,
        }
    }

    /// Start/poll one source-owned worker.  The worker reads and clones every
    /// source itself; this UI-tick method never flattens an application
    /// corpus before spawn, and a query never starts it.
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
        let spawned = std::thread::Builder::new()
            .name("tilo-app-search-index".into())
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
            });
        if spawned.is_ok() {
            *self.build.borrow_mut() = Some(receiver);
        }
    }

    fn snapshot(&self) -> Option<Ref<'_, ApplicationsSnapshot>> {
        let generation = self.source_generation();
        let snapshot = self.snapshot.borrow();
        Ref::filter_map(snapshot, Option::as_ref)
            .ok()
            .filter(|snapshot| snapshot.source_generation == generation)
    }

    /// Score `apps` against `needle`. `open_target` only words the row —
    /// the launch reads the live target again at activation time, since it
    /// can change between the query that built this row and the Enter that
    /// fires it.
    pub(crate) fn results_for(
        &self,
        needle: &str,
        apps: Vec<InstalledApp>,
        open_target: OpenTarget,
    ) -> Vec<SearchResult> {
        let needle_lower = needle.to_lowercase();
        let kind = ResourceKind::Application;
        let mut out = Vec::new();
        for app in apps {
            let name_lower = app.display_name.to_lowercase();
            let id_lower = app.package_id.to_lowercase();
            let category_lower = app.category.as_deref().map(str::to_lowercase);
            let Some(score) = match_score(
                &needle_lower,
                &name_lower,
                &id_lower,
                category_lower.as_deref(),
                None,
            ) else {
                continue;
            };
            let package_id = app.package_id.clone();
            let display_name = app.display_name.clone();
            // A declared category is a more useful second line than the version.
            let detail = app_detail(needle, &app);
            // Where the window will land leads the line, since it's the
            // part of a launch row that changes with context.
            let subtitle = format!("{} · {detail}", open_target.result_hint());
            out.push(SearchResult::nullary_fallible(
                kind.clone(),
                self.id(),
                app.package_id.clone(),
                format!("Open {}", app.display_name),
                Some(subtitle),
                presentation_score(needle, &app.package_id, score),
                move |_doc, _root, chrome: &Chrome| {
                    launch_installed_app(chrome, &package_id, &display_name)
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
        self.dynamic_results_for_membership(needle, records, &installed_gui, open_target)
    }

    fn dynamic_results_for_membership(
        &self,
        needle: &str,
        records: Vec<DynamicLauncherRecord>,
        installed_gui: &HashSet<String>,
        open_target: OpenTarget,
    ) -> Vec<SearchResult> {
        let needle_lower = needle.to_lowercase();
        records
            .into_iter()
            .filter_map(|record| {
                let name_lower = record.name.to_lowercase();
                let id_lower = record.id.to_lowercase();
                let score = match_score(&needle_lower, &name_lower, &id_lower, None, None)?;
                let valid = match record.launcher_type {
                    1 => record.target.is_none()
                        && installed_gui.contains(&record.owner_app_id.to_ascii_lowercase()),
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
                    format!("Open {}", record.name),
                    Some(format!("{} · Shortcut", open_target.result_hint())),
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
                                        format!("Couldn't open {name}"),
                                        // The URI resolver's Debug output may
                                        // contain package ids, paths, or
                                        // multiline backend details. Keep the
                                        // full value in the refusal returned
                                        // to the caller/log and give the
                                        // fixed-height toast a bounded recovery
                                        // instruction instead.
                                        "The shortcut could not be opened. Check its target and try again.",
                                    )
                                    .with_severity(NotificationSeverity::Error)
                                    .with_app_name("Applications"),
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
        self.catalogue_results_for_membership(
            needle,
            apps,
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
        installed_ids: &HashSet<String>,
        host_app_ids: &HashSet<String>,
        _open_target: OpenTarget,
    ) -> Vec<SearchResult> {
        let needle_lower = needle.to_lowercase();
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        for app in apps {
            let package_id_key = app.package_id.to_ascii_lowercase();
            if installed_ids.contains(&package_id_key)
                || host_app_ids.contains(&package_id_key)
                || !seen.insert(package_id_key)
            {
                continue;
            }
            let name_lower = app.display_name.to_lowercase();
            let id_lower = app.package_id.to_lowercase();
            let category_lower = app.category.as_deref().map(str::to_lowercase);
            let description_lower = app.description.to_lowercase();
            let Some(score) = match_score(
                &needle_lower,
                &name_lower,
                &id_lower,
                category_lower.as_deref(),
                Some(&description_lower),
            ) else {
                continue;
            };
            let package_id = app.package_id.clone();
            let display_name = app.display_name.clone();
            let detail = catalogue_app_detail(needle, &app);
            let trust = if app.trust.trim().is_empty() {
                "Registry verification pending".to_string()
            } else {
                app.trust.clone()
            };
            let subtitle = format!("App Store · {trust} · {detail}");
            out.push(SearchResult {
                kind: ResourceKind::Application,
                provider_id: self.id().to_string(),
                id: format!("app:{}", app.package_id),
                title: format!("View {} in App Store", app.display_name),
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
                        tree.active_workspace()
                            .map(|ws| (ws.room, ws.capabilities.clone()))
                    };
                    let Some((room, capabilities)) = context else {
                        // No active workspace is no room to launch into —
                        // reported exactly as an installed app's launch would.
                        let mut st = chrome.borrow_mut();
                        st.push_notification_full(
                            Notification::new(
                                format!("Couldn't open App Store for {display_name}"),
                                launcher::LaunchError::NoWorkspace.notification_message(),
                            )
                            .with_severity(NotificationSeverity::Error)
                            .with_app_name("Applications")
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
                                    format!("Couldn't open App Store for {display_name}"),
                                    message.clone(),
                                )
                                .with_severity(NotificationSeverity::Error)
                                .with_app_name("Applications"),
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
        if st.tree().active_workspace().is_none() {
            return;
        }
        st.tree().active
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
    let Some(home) = dirs::home_dir() else {
        return false;
    };
    let Some(ws) = tree.workspaces.iter_mut().find(|w| w.id == workspace) else {
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
    ws.grant_app(
        package_id,
        &Capability::Filesystem {
            path: home,
            mode: FsMode::ReadWrite,
            scope: GRANT_SESSION,
        },
    );
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
    grant_user_home_to_file_manager(chrome, package_id);
    // If the room already has this app open, focus it rather than starting
    // a second copy. See `peer_windows::opened_for_the_room`.
    let already = {
        let st = chrome.borrow();
        let tree = st.tree();
        let salt = tree.local_salt();
        tree.active_workspace().and_then(|ws| {
            crate::shell::peer_windows::opened_for_the_room(ws, package_id, salt)
                .map(|(strip, group, tab)| (ws.id, strip, group, tab))
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
            Notification::new(format!("Opening {display_name}"), "Loading…".to_string())
                .with_app_name("Applications")
                .with_dismiss_after(std::time::Duration::from_secs(120)),
        );
        st.bump();
        id
    };
    catalogue::note_pending_notice(package_id, notice);
    // Scoped so the borrow is released before the error branch below
    // takes a mutable one.
    let launched = {
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
        tree.active_workspace().map(|ws| {
            launcher::set_workspace_capabilities(ws.capabilities.clone());
            launcher::launch_in_workspace(package_id, ws)
        })
    };
    // Non-blocking past resolution — only pre-fork failures land here, the
    // rest arrive as launch events drained each frame.
    let outcome = launched.unwrap_or(Err(launcher::LaunchError::NoWorkspace));
    if let Err(err) = outcome {
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
                .with_app_name("Applications")
                .with_dismiss_after(launcher::LAUNCH_FAILURE_NOTICE_LIFETIME),
            );
            st.bump();
        }
        // How a caller that cannot see the screen learns the launch failed.
        return Err(ActionError::Refused(format!(
            "Couldn't open {display_name}: {message}"
        )));
    }
    Ok(())
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
        self.refresh_snapshot();
    }

    fn search_revision(&self, _ctx: &SearchContext<'_>) -> String {
        self.revision()
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
        let after = match cursor {
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
            let Some(entry) = snapshot.entries.get(id) else {
                continue;
            };
            let mut rows = match entry {
                ApplicationEntry::Installed(app) => {
                    self.results_for(ctx.needle, vec![app.clone()], ctx.chrome.open_target)
                }
                ApplicationEntry::Dynamic(record) => self.dynamic_results_for_membership(
                    ctx.needle,
                    vec![record.clone()],
                    &snapshot.installed_ids,
                    ctx.chrome.open_target,
                ),
                ApplicationEntry::Catalogue(app) => self.catalogue_results_for_membership(
                    ctx.needle,
                    vec![app.clone()],
                    &snapshot.installed_ids,
                    &snapshot.host_app_ids,
                    ctx.chrome.open_target,
                ),
            };
            let Some(row) = rows.pop() else { continue };
            let row_bytes = row
                .title
                .len()
                .saturating_add(row.subtitle.as_ref().map_or(0, String::len));
            if bytes.saturating_add(row_bytes) > budget.max_bytes {
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
                token: format!("applications:{offset}\u{1f}{}", ctx.needle),
            }),
            exhausted,
            stale_cursor: false,
            budget_exhausted: !exhausted,
        }
    }
}

impl ApplicationsProvider {
    fn revision(&self) -> String {
        let (installed, catalogue, host, dynamic) = self.source_generation();
        let ready = self.snapshot.borrow().as_ref().is_some_and(|snapshot| {
            snapshot.source_generation == (installed, catalogue, host, dynamic)
        });
        format!(
            "{}:{installed}:{catalogue}:{host}:{dynamic}",
            if ready { "ready" } else { "indexing" }
        )
    }
}

/// Case-insensitive, first matching band wins in the order written below:
/// name prefix 100, id in full or last segment 100, name substring 50, id
/// prefix 60, id substring 25, category substring 20, description 15,
/// fuzzy subsequence on the name 10. Multi-word queries may span metadata
/// fields, so `office spreadsheet` can find an app whose category and
/// description carry one word each. Empty needle matches everything at 0, so an empty palette
/// lists installed apps. Category ranks below the id because it's a bucket,
/// not an identity. Package ids score by how completely they were named,
/// not flat "contains" — `hex-editor` typed for `so.tilo.hex-editor` is the
/// app's identity in full and must outrank an incidental substring.
pub(crate) fn match_score(
    needle: &str,
    name: &str,
    package_id: &str,
    category: Option<&str>,
    description: Option<&str>,
) -> Option<i32> {
    if needle.is_empty() {
        return Some(0);
    }
    if name.starts_with(needle) {
        return Some(100);
    }
    // The id in full, or its last segment — both identify exactly one app.
    if package_id == needle || package_id.rsplit('.').next() == Some(needle) {
        return Some(100);
    }
    if name.contains(needle) {
        return Some(50);
    }
    if package_id.starts_with(needle) {
        return Some(60);
    }
    if package_id.contains(needle) {
        return Some(25);
    }
    if category.is_some_and(|c| c.contains(needle)) {
        return Some(20);
    }
    if description.is_some_and(|d| d.contains(needle)) {
        return Some(15);
    }
    let terms: Vec<&str> = needle.split_whitespace().collect();
    if terms.len() > 1
        && terms.iter().all(|term| {
            name.contains(term)
                || package_id.contains(term)
                || category.is_some_and(|value| value.contains(term))
                || description.is_some_and(|value| value.contains(term))
        })
    {
        return Some(15);
    }
    if fuzzy_subsequence(needle, name) {
        return Some(10);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::chrome_state::ChromeState;
    use crate::shell::state::{WorkspaceTree, WorkspaceTreeHandle};
    use automerge::AutoCommit;
    use parking_lot::RwLock;
    use std::path::PathBuf;
    use std::sync::Arc;

    fn chrome_rc() -> Chrome {
        let handle: WorkspaceTreeHandle = Arc::new(RwLock::new(WorkspaceTree::default()));
        ChromeState::new_with_handle(handle)
    }

    fn refresh_until_ready(provider: &ApplicationsProvider, ctx: &SearchContext<'_>) {
        for _ in 0..200 {
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
    #[serial_test::serial(launch_events)]
    fn repeated_activation_while_opening_is_idempotent() {
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
    }

    #[test]
    fn matching_app_offers_an_open_row() {
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
            Some("Opens beside · Tilo · so.tilo.hello · 0.1.0"),
            "the row names the package it will launch"
        );
    }

    #[test]
    fn available_apps_are_discoverable_by_description_and_cross_field_terms() {
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

    #[test]
    fn duplicate_registry_offers_produce_one_application_row() {
        let p = ApplicationsProvider::new();
        let app = offered("so.tilo.notes", "Notes", Some("Office"));
        let out = p.catalogue_results_for("notes", vec![app.clone(), app], &[], OpenTarget::Beside);
        assert_eq!(out.len(), 1, "registry mirrors must not duplicate an app");
    }

    #[test]
    fn an_exact_host_package_identity_hides_its_catalogue_alias() {
        let p = ApplicationsProvider::new();
        let hosts = vec![crate::shell::host_apps::HostApp {
            desktop_id: "so.tilo.notes".into(),
            name: "Notes".into(),
            source_label: "Flatpak",
            categories: Vec::new(),
            keywords: Vec::new(),
            window_identities: vec!["so.tilo.notes".into()],
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
    fn gnome_calendar_does_not_hide_tilo_calendar() {
        let rows = catalogue_rows_with_host(
            "so.tilo.calendar",
            "Calendar",
            "org.gnome.Calendar",
            "Calendar",
        );
        assert_eq!(rows.len(), 1);
        assert!(rows[0].subtitle.as_deref().unwrap().contains("Tilo"));
    }

    #[test]
    fn host_terminal_does_not_hide_tilo_terminal() {
        let rows = catalogue_rows_with_host(
            "so.tilo.terminal",
            "Terminal",
            "org.gnome.Console",
            "Terminal",
        );
        assert_eq!(rows.len(), 1);
    }

    #[test]
    fn a_generic_shared_name_is_not_cross_provider_identity() {
        let rows = catalogue_rows_with_host(
            "so.tilo.notes",
            "Notes",
            "org.example.PrivateNotes",
            "Notes",
        );
        assert_eq!(rows.len(), 1);
    }

    #[test]
    fn declared_category_is_matchable_and_replaces_the_version_subtitle() {
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
        assert_eq!(out[0].score, 20, "category ranks below name and id matches");
        assert_eq!(
            out[0].subtitle.as_deref(),
            Some("Opens beside · Tilo · Office · so.tilo.sheets"),
            "a declared category is the more useful second line than the version"
        );
    }

    #[test]
    fn category_never_outranks_a_name_or_id_match() {
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
        assert_eq!(score("so.tilo.monitor"), 20, "category-only match");
        assert!(score("so.tilo.sysinfo") > score("so.tilo.monitor"));
    }

    #[test]
    fn package_id_matches_score_below_name_matches() {
        let p = ApplicationsProvider::new();
        let out = p.results_for(
            "so.tilo",
            vec![app("so.tilo.hello", "Hello"), app("org.x.zed", "Zed")],
            OpenTarget::Beside,
        );
        assert_eq!(out.len(), 1, "only the matching package: {out:?}");
        // A shared id *prefix* names a publisher, not an app, so it
        // stays below a name match.
        assert_eq!(out[0].score, 60);
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

    /// The display name is "Hex Editor", which never prefix-matches the id.
    #[test]
    fn the_id_last_segment_is_an_identity_match() {
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

    #[test]
    fn non_matching_needle_returns_nothing() {
        let p = ApplicationsProvider::new();
        let out = p.results_for(
            "zzzqqq",
            vec![app("so.tilo.hello", "Hello")],
            OpenTarget::Beside,
        );
        assert!(out.is_empty());
    }

    #[test]
    fn empty_needle_lists_every_app_with_a_useful_fresh_system_order() {
        let p = ApplicationsProvider::new();
        let out = p.results_for(
            "",
            vec![
                app("so.tilo.monitor", "System Monitor"),
                app("so.tilo.browser", "Browser"),
                app("so.tilo.notes", "Notes"),
                app("so.tilo.hello", "Hello"),
            ],
            OpenTarget::Beside,
        );
        assert_eq!(out.len(), 4);
        let score = |id: &str| out.iter().find(|row| row.id == id).unwrap().score;
        assert!(score("so.tilo.browser") > score("so.tilo.notes"));
        assert!(score("so.tilo.notes") > score("so.tilo.monitor"));
        assert!(score("so.tilo.monitor") > score("so.tilo.hello"));
        assert!(
            out.iter()
                .find(|row| row.id == "so.tilo.browser")
                .and_then(|row| row.subtitle.as_deref())
                .is_some_and(|subtitle| subtitle.contains("Start here")),
            "the ordering must also be explained on screen"
        );
    }

    #[test]
    fn typed_search_ignores_default_presentation_tiers() {
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

    #[test]
    fn unrequested_kind_returns_empty() {
        let p = ApplicationsProvider::new();
        let c = chrome_rc();
        let cs = c.borrow();
        let doc = AutoCommit::new();
        let out = p.query(SearchContext {
            needle: "hello",
            wanted: &[ResourceKind::File],
            chrome: &cs,
            doc: &doc,
            doc_root: &automerge::ROOT,
        });
        assert!(out.is_empty());
    }

    /// A launch that cannot start must reach both audiences: a notification
    /// for the screen, and a refusal for the caller that invoked the row by
    /// address — without it, a script is told the app opened and waits for
    /// a window that never arrives. Drives the `on_open` closure.
    #[test]
    fn a_failed_launch_notifies_the_user_and_refuses_the_caller() {
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
            &mut doc,
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

    #[test]
    fn an_uninstalled_app_opens_its_canonical_store_detail() {
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

    #[test]
    fn palette_result_never_installs_and_store_uri_is_stable() {
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

    #[test]
    fn uninstalled_rows_are_explicit_about_the_store_review_surface() {
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

    #[test]
    fn an_app_you_have_wins_a_tie_but_not_a_better_match() {
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
            vec![app("so.tilo.shed", "Shed Helper Editor Extra Tool Set")],
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

    /// The same app can be in both lists briefly; must produce one row, not two.
    #[test]
    fn an_app_that_just_became_installed_is_not_offered_twice() {
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

    #[test]
    fn a_non_matching_needle_offers_nothing_from_the_catalogue() {
        let p = ApplicationsProvider::new();
        let out = p.catalogue_results_for(
            "zzzqqq",
            vec![offered("so.tilo.sheets", "Sheets", None)],
            &[],
            OpenTarget::Beside,
        );
        assert!(out.is_empty());
    }

    /// Must say so in the same words an installed app's launch uses, not
    /// fail silently. Drives the `on_open` closure.
    #[test]
    #[serial_test::serial(launch_events)]
    fn activating_with_no_workspace_reports_it_the_same_way() {
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
        let opened = (row.on_open)(&mut doc, &automerge::ROOT, &c, &ActionArgs::none());
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

    #[test]
    fn shell_query_includes_an_uninstalled_app_as_a_store_detail_row() {
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

    #[test]
    fn cold_and_warm_public_pages_never_rescan_a_hundred_thousand_apps() {
        let chrome = chrome_rc();
        let chrome = chrome.borrow();
        let doc = AutoCommit::new();
        let ctx = SearchContext {
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
            cold.build.borrow().is_none(),
            "a public cold query must not start or clone a full application corpus"
        );

        let provider = ApplicationsProvider::new();
        let entries = (0..100_001)
            .map(|index| {
                ApplicationEntry::Installed(app_in(
                    &format!("so.tilo.fixture-{index}"),
                    if index == 100_000 {
                        "unique-tail"
                    } else {
                        "common app"
                    },
                    None,
                ))
            })
            .collect::<Vec<_>>();
        let index = ui::search::SearchTextIndex::new(entries.iter().map(|entry| match entry {
            ApplicationEntry::Installed(app) => {
                format!("{}\u{1f}{}", app.display_name, app.package_id)
            }
            ApplicationEntry::Dynamic(_) | ApplicationEntry::Catalogue(_) => String::new(),
        }));
        *provider.snapshot.borrow_mut() = Some(ApplicationsSnapshot {
            source_generation: provider.source_generation(),
            entries,
            installed_ids: HashSet::new(),
            host_app_ids: HashSet::new(),
            index,
        });
        let page = provider.query_page(
            SearchContext {
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
        assert!(provider.build.borrow().is_none());
    }

    #[test]
    fn a_worker_snapshot_is_rejected_when_its_source_revision_changed() {
        let source = ApplicationsTestSource::new(Vec::new(), Vec::new(), Vec::new(), Vec::new());
        let provider = ApplicationsProvider::with_test_source(Arc::clone(&source));
        let chrome = chrome_rc();
        let chrome = chrome.borrow();
        let doc = AutoCommit::new();
        let ctx = SearchContext {
            needle: "anything",
            wanted: &[ResourceKind::Application],
            chrome: &chrome,
            doc: &doc,
            doc_root: &automerge::ROOT,
        };
        let initial_revision = provider.search_revision(&ctx);
        provider.refresh_search_index(&ctx);
        for _ in 0..200 {
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
    fn source_backed_cold_query_does_no_application_index_work_before_refresh() {
        let source = ApplicationsTestSource::new(
            Vec::new(),
            (0..100_001)
                .map(|index| CatalogueApp {
                    package_id: format!("so.tilo.cold-fixture-{index}"),
                    display_name: if index == 100_000 {
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
                .expect("application build thread lock"),
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

    /// A state that flashes for a hundred milliseconds is noise, not feedback.
    #[test]
    fn an_app_that_is_already_here_does_not_raise_a_loading_state() {
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

    #[test]
    fn reviewed_web_shortcut_is_searchable_but_malformed_records_are_hidden() {
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

/// Tiny session-owned catalog of apps the native host can actually construct.
/// It shares application visibility policy and action dispatch with package apps.
pub struct NativeApplicationsProvider;

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
        let needle = ctx.needle.to_lowercase();
        ctx.chrome
            .hosted_inputs
            .native_apps()
            .iter()
            .filter_map(|app| {
                let score = match_score(
                    &needle,
                    &app.display_name.to_lowercase(),
                    &app.package_id.to_lowercase(),
                    None,
                    None,
                )?;
                let id = app.package_id;
                Some(SearchResult::nullary_fallible(
                    ResourceKind::Application,
                    self.id(),
                    id,
                    format!("Open {}", app.display_name),
                    Some(ctx.chrome.open_target.result_hint().into()),
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
                            OpenTarget::Beside => crate::shell::window_actions::Destination::Beside,
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
    fn native_files_search_opens_the_real_hosted_app_and_revalidates_availability() {
        let directory = tempfile::tempdir().unwrap();
        let chrome = ChromeState::new(WorkspaceTree::bootstrap_default());
        chrome.borrow_mut().hosted_inputs =
            HostedTileInputs::for_host_directory(directory.path()).unwrap();
        let mut doc = automerge::AutoCommit::new();
        let results = NativeApplicationsProvider.query(SearchContext {
            needle: "Files",
            wanted: &[ResourceKind::Application],
            chrome: &chrome.borrow(),
            doc: &doc,
            doc_root: &automerge::ROOT,
        });
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "org.tilo.Files");
        (results[0].on_open)(&mut doc, &automerge::ROOT, &chrome, &ActionArgs::none()).unwrap();
        assert_eq!(chrome.borrow().active_app_title().as_deref(), Some("Files"));
        assert!(
            chrome
                .borrow()
                .tree()
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
            (results[0].on_open)(&mut doc, &automerge::ROOT, &chrome, &ActionArgs::none()).is_err()
        );
    }
}
