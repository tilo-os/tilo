//! Apps a person can get, from the catalogue search shares with the Store
//! ([`app_catalogue`]): Tilo apps the registries offer and Flathub apps from
//! Flatpak's cached metadata, plus every Tilo app's documents and closed
//! actions, whether or not the app is on this device.
//!
//! - A **Tilo app** has no install step. Enter opens it; when it is absent
//!   it downloads first, through the on-demand path a missing app's window
//!   takes ([`catalogue::install_and_launch`]), with its place held by the
//!   opening card.
//! - A **Flathub app** is installed explicitly: Enter installs it and opens
//!   it when that is done. Its other action shows it in the App Store.
//! - A Tilo app's **document** ("Workbook") opens the app, which starts on a
//!   new one; its **action** ("Show tasks") gets the app if needed and runs
//!   through the session's action host, which starts the app as it does for
//!   any closed app's offered action.
//!
//! Rows rank just below installed apps of equal match, never stand for an
//! app the device already has (a Tilo package, a host or Flatpak app), and
//! never show an app that is blocked.

use std::cell::Ref;
use std::collections::HashMap;
use std::path::PathBuf;
#[cfg(test)]
use std::sync::Arc;

use app_catalogue::{Catalogue, CatalogueEntry, Installed, Source};
use tilo_desktop::notifications::{Notification, NotificationSeverity};
use tilo_search::{Document, Matching, SearchIndex};
use ui::search::FieldWeight;

use super::background_index::BackgroundIndex;
use crate::shell::catalogue::{self, CatalogueApp, SharedCatalogue};
use crate::shell::certified_action_session::ActionSessionSource;
use crate::shell::chrome_state::Chrome;
use crate::shell::i18n::{t, t_args};
use crate::shell::launcher;
use crate::shell::search::action_api::ActionError;
use crate::shell::search::{
    ResourceKind, SearchBudget, SearchContext, SearchCursor, SearchPage, SearchProvider,
    SearchResult, provider_score,
};

/// The provider's id, which also keys what was chosen from it.
pub const PROVIDER_ID: &str = "store-catalogue";

/// An app the device does not have ranks one point below one it has, at
/// an equal match: enough to break the tie, never enough to put a worse
/// match of an installed app above a better one of an offered app.
const NOT_INSTALLED_TIEBREAK: i32 = 1;

const STORE_PACKAGE_ID: &str = "so.tilo.store";

/// Row ids: a Tilo app keeps the `app:` address its Store offer always had.
const TILO_PREFIX: &str = "app:";
const FLATHUB_PREFIX: &str = "flathub:";
/// `new:<app>/<resource kind>`.
const DOCUMENT_PREFIX: &str = "new:";
/// `offer:<app>/<resource>/<action>`.
const ACTION_PREFIX: &str = "offer:";
/// The "Show in App Store" row of a Flathub app's other actions.
const STORE_PAGE_PREFIX: &str = "store-page:";

/// Matching like the installed apps': words spread over fields, typos in
/// names.
const CATALOGUE_MATCHING: Matching = Matching::LOOSE;

/// (installed apps, Tilo registries, Flathub and blocks, host apps).
type Generation = (u64, u64, u64, u64);

/// One searchable row of the snapshot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Row {
    App(usize),
    Document { entry: usize, kind: usize },
    Action { entry: usize, action: usize },
}

struct CatalogueSnapshot {
    /// The offered entries: neither installed nor blocked.
    entries: Vec<CatalogueEntry>,
    rows: Vec<Row>,
    /// Cached icon files, by row id.
    icons: HashMap<String, PathBuf>,
    /// Rows' fields, keyed by position in `rows`.
    index: SearchIndex,
}

#[cfg(test)]
#[derive(Clone, Default)]
pub(crate) struct StoreCatalogueTestState {
    /// Ids of the apps the device has: Tilo packages and host apps.
    pub installed: Vec<String>,
    pub tilo: Vec<CatalogueApp>,
    pub shared: SharedCatalogue,
}

#[cfg(test)]
pub(crate) struct StoreCatalogueTestSource {
    generation: std::sync::atomic::AtomicU64,
    state: std::sync::Mutex<StoreCatalogueTestState>,
}

#[cfg(test)]
impl StoreCatalogueTestSource {
    pub(crate) fn new(state: StoreCatalogueTestState) -> Arc<Self> {
        Arc::new(Self {
            generation: std::sync::atomic::AtomicU64::new(1),
            state: std::sync::Mutex::new(state),
        })
    }

    fn generation(&self) -> Generation {
        let generation = self.generation.load(std::sync::atomic::Ordering::Acquire);
        (generation, generation, generation, generation)
    }

    pub(crate) fn replace(&self, state: StoreCatalogueTestState) {
        *self.state.lock().expect("catalogue test source lock") = state;
        self.generation
            .fetch_add(1, std::sync::atomic::Ordering::Release);
    }
}

pub struct StoreCatalogueProvider {
    kinds: Vec<ResourceKind>,
    index: BackgroundIndex<Generation, CatalogueSnapshot>,
    /// How a closed app's offered action is run; `None` until the session's
    /// action broker is attached.
    action_sessions: Option<ActionSessionSource>,
    #[cfg(test)]
    source: Option<Arc<StoreCatalogueTestSource>>,
}

impl Default for StoreCatalogueProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl StoreCatalogueProvider {
    pub fn new() -> Self {
        Self {
            kinds: vec![ResourceKind::Application, ResourceKind::Action],
            index: BackgroundIndex::new("tilo-catalogue-search-index"),
            action_sessions: None,
            #[cfg(test)]
            source: None,
        }
    }

    /// The provider once the session's action broker is attached: offered
    /// actions of apps not on this device run through it.
    pub(crate) fn with_action_sessions(sessions: ActionSessionSource) -> Self {
        Self {
            action_sessions: Some(sessions),
            ..Self::new()
        }
    }

    #[cfg(test)]
    pub(crate) fn with_test_source(source: Arc<StoreCatalogueTestSource>) -> Self {
        Self {
            source: Some(source),
            ..Self::new()
        }
    }

    #[allow(clippy::unused_self, reason = "test builds use the injected source")]
    fn source_generation(&self) -> Generation {
        #[cfg(test)]
        if let Some(source) = &self.source {
            return source.generation();
        }
        (
            launcher::gui_apps_generation(),
            catalogue::available_apps_generation(),
            catalogue::shared_generation(),
            crate::shell::host_apps::host_apps_generation(),
        )
    }

    /// Read every source and index the offered rows. Runs on the index's
    /// worker, never on a keystroke.
    fn build_snapshot(
        #[cfg(test)] source: Option<Arc<StoreCatalogueTestSource>>,
    ) -> (Generation, CatalogueSnapshot) {
        #[cfg(test)]
        if let Some(source) = source {
            let generation = source.generation();
            let state = source
                .state
                .lock()
                .expect("catalogue test source lock")
                .clone();
            let installed = Installed::from_ids(&state.installed);
            return (
                generation,
                CatalogueSnapshot::from_parts(&installed, &state.tilo, &state.shared),
            );
        }
        let (installed_generation, installed_apps) = launcher::gui_apps_snapshot();
        let (tilo_generation, tilo) = catalogue::available_apps_snapshot();
        let (shared_generation, shared) = catalogue::shared_snapshot();
        let (host_generation, host_apps) = crate::shell::host_apps::host_apps_snapshot();
        let installed = Installed::from_ids(
            installed_apps
                .iter()
                .map(|app| app.package_id.as_str())
                .chain(host_apps.iter().flat_map(|app| {
                    std::iter::once(app.desktop_id.as_str())
                        .chain(app.window_identities.iter().map(String::as_str))
                })),
        );
        (
            (
                installed_generation,
                tilo_generation,
                shared_generation,
                host_generation,
            ),
            CatalogueSnapshot::from_parts(&installed, &tilo, &shared),
        )
    }

    #[allow(clippy::unused_self, reason = "test builds use the injected source")]
    fn builder(&self) -> impl FnOnce() -> (Generation, CatalogueSnapshot) + Send + 'static {
        #[cfg(test)]
        let source = self.source.clone();
        move || {
            Self::build_snapshot(
                #[cfg(test)]
                source,
            )
        }
    }

    fn snapshot(&self) -> Option<Ref<'_, CatalogueSnapshot>> {
        self.index.current(&self.source_generation())
    }

    fn revision(&self) -> String {
        let generation = self.source_generation();
        let (installed, tilo, shared, host) = generation;
        let ready = self.index.is_ready(&generation);
        format!(
            "{}:{installed}:{tilo}:{shared}:{host}",
            if ready { "ready" } else { "indexing" }
        )
    }

    /// The row `row` of `snapshot`, as a search shows it.
    fn row(&self, snapshot: &CatalogueSnapshot, row: Row, score: i32) -> Option<SearchResult> {
        let score = score.saturating_sub(NOT_INSTALLED_TIEBREAK).max(1);
        match row {
            Row::App(at) => Some(app_row(snapshot.entries.get(at)?, score)),
            Row::Document { entry, kind } => {
                let entry = snapshot.entries.get(entry)?;
                let kind = entry.resource_types.get(kind)?;
                Some(document_row(entry, kind, score))
            }
            Row::Action { entry, action } => {
                let entry = snapshot.entries.get(entry)?;
                let action = entry.actions.get(action)?;
                Some(action_row(
                    entry,
                    action,
                    self.action_sessions.clone(),
                    score,
                ))
            }
        }
    }
}

impl CatalogueSnapshot {
    fn from_parts(installed: &Installed, tilo: &[CatalogueApp], shared: &SharedCatalogue) -> Self {
        let catalogue = Catalogue::from_sources(
            tilo.iter().map(app_catalogue::tilo::entry).collect(),
            shared.flathub.as_ref().clone(),
        );
        let entries: Vec<CatalogueEntry> = catalogue
            .offered(installed, &shared.blocks)
            .cloned()
            .collect();
        let mut rows = Vec::new();
        for (at, entry) in entries.iter().enumerate() {
            rows.push(Row::App(at));
            // Only a Tilo app opens on a document or runs an action without
            // an install first; a Flathub app has neither in its metadata.
            if matches!(entry.source, Source::Tilo { .. }) {
                rows.extend(
                    (0..entry.resource_types.len()).map(|kind| Row::Document { entry: at, kind }),
                );
                rows.extend(
                    (0..entry.actions.len()).map(|action| Row::Action { entry: at, action }),
                );
            }
        }
        let icons = entries
            .iter()
            .filter_map(|entry| Some((app_row_id(entry), entry.icon_file.clone()?)))
            .collect();
        let index = index_rows(&entries, &rows);
        Self {
            entries,
            rows,
            icons,
            index,
        }
    }

    /// The id of `row`, without building it.
    fn row_id(&self, row: Row) -> Option<String> {
        Some(match row {
            Row::App(at) => app_row_id(self.entries.get(at)?),
            Row::Document { entry, kind } => {
                let entry = self.entries.get(entry)?;
                document_row_id(&entry.id, &entry.resource_types.get(kind)?.kind)
            }
            Row::Action { entry, action } => {
                let entry = self.entries.get(entry)?;
                let action = entry.actions.get(action)?;
                action_row_id(&entry.id, &action.resource, &action.action)
            }
        })
    }
}

fn app_row_id(entry: &CatalogueEntry) -> String {
    match entry.source {
        Source::Tilo { .. } => format!("{TILO_PREFIX}{}", entry.id),
        Source::Flathub { .. } => format!("{FLATHUB_PREFIX}{}", entry.id),
    }
}

fn document_row_id(app: &str, kind: &str) -> String {
    format!("{DOCUMENT_PREFIX}{app}/{kind}")
}

fn action_row_id(app: &str, resource: &str, action: &str) -> String {
    format!("{ACTION_PREFIX}{app}/{resource}/{action}")
}

/// The app a row of this provider is for.
fn app_of_row(id: &str) -> Option<&str> {
    if let Some(app) = id
        .strip_prefix(TILO_PREFIX)
        .or_else(|| id.strip_prefix(FLATHUB_PREFIX))
        .or_else(|| id.strip_prefix(STORE_PAGE_PREFIX))
    {
        return Some(app);
    }
    id.strip_prefix(DOCUMENT_PREFIX)
        .or_else(|| id.strip_prefix(ACTION_PREFIX))
        .and_then(|rest| rest.split('/').next())
}

/// Each row's searchable fields, keyed by its position.
fn index_rows(entries: &[CatalogueEntry], rows: &[Row]) -> SearchIndex {
    let mut index =
        SearchIndex::new(CATALOGUE_MATCHING).with_ranks(ui::search::system::application_ranks);
    index.replace_all(rows.iter().enumerate().filter_map(|(at, row)| {
        let fields = row_fields(entries, *row)?;
        Some(
            fields
                .into_iter()
                .fold(Document::new(at as u64), |document, (text, weight)| {
                    document.field(text, weight)
                }),
        )
    }));
    if let Err(error) = index.commit() {
        tracing::error!(%error, "could not index the app catalogue");
    }
    index
}

/// What a row is found by: an app by everything it is called and known by,
/// as an installed app is; a document or action by its own name first and
/// its app's after.
fn row_fields(entries: &[CatalogueEntry], row: Row) -> Option<Vec<(String, FieldWeight)>> {
    Some(match row {
        Row::App(at) => {
            let entry = entries.get(at)?;
            super::applications::searchable_fields(
                &entry.name,
                &entry.id,
                entry.category.as_deref(),
                (!entry.tagline.is_empty()).then_some(entry.tagline.as_str()),
                entry.other_names.iter().cloned(),
                &entry.keywords,
            )
        }
        Row::Document { entry, kind } => {
            let entry = entries.get(entry)?;
            let kind = entry.resource_types.get(kind)?;
            vec![
                (kind.label.clone(), FieldWeight::TITLE),
                (entry.name.clone(), FieldWeight::SUBTITLE),
            ]
        }
        Row::Action { entry, action } => {
            let entry = entries.get(entry)?;
            let action = entry.actions.get(action)?;
            let mut fields = vec![
                (action.label.clone(), FieldWeight::TITLE),
                (entry.name.clone(), FieldWeight::SUBTITLE),
            ];
            fields.extend(
                action
                    .description
                    .clone()
                    .map(|description| (description, FieldWeight::METADATA)),
            );
            fields
        }
    })
}

/// An app row's second line: what the app is, or where it comes from when
/// its source says nothing about it.
fn app_subtitle(entry: &CatalogueEntry) -> String {
    if !entry.tagline.trim().is_empty() {
        return entry.tagline.trim().to_owned();
    }
    match entry.source {
        Source::Tilo { .. } => t("search-catalogue-source-tilo"),
        Source::Flathub { .. } => t("search-catalogue-source-flathub"),
    }
}

fn app_row(entry: &CatalogueEntry, score: i32) -> SearchResult {
    let id = entry.id.clone();
    let name = entry.name.clone();
    match entry.source {
        Source::Tilo { .. } => SearchResult::nullary_fallible(
            ResourceKind::Application,
            PROVIDER_ID,
            app_row_id(entry),
            entry.name.clone(),
            Some(app_subtitle(entry)),
            score,
            move |_doc, _root, chrome: &Chrome| open_tilo_app(chrome, &id, &name),
        ),
        Source::Flathub { .. } => {
            let mut row = SearchResult::nullary_fallible(
                ResourceKind::Application,
                PROVIDER_ID,
                app_row_id(entry),
                entry.name.clone(),
                Some(app_subtitle(entry)),
                score,
                move |_doc, _root, chrome: &Chrome| install_flathub_app(chrome, &id, &name),
            );
            // Where it comes from is said once: beside the tag line, or as
            // the second line when there is no tag line.
            if !entry.tagline.trim().is_empty() {
                row.badge = Some(t("search-catalogue-source-flathub"));
            }
            row
        }
    }
}

fn document_row(
    entry: &CatalogueEntry,
    kind: &app_catalogue::ResourceType,
    score: i32,
) -> SearchResult {
    let id = entry.id.clone();
    let name = entry.name.clone();
    SearchResult::nullary_fallible(
        ResourceKind::Action,
        PROVIDER_ID,
        document_row_id(&entry.id, &kind.kind),
        kind.label.clone(),
        Some(t_args(
            "search-catalogue-document-subtitle",
            &[("app", entry.name.as_str().into())],
        )),
        score,
        move |_doc, _root, chrome: &Chrome| open_tilo_app(chrome, &id, &name),
    )
}

fn action_row(
    entry: &CatalogueEntry,
    action: &app_catalogue::CatalogueAction,
    sessions: Option<ActionSessionSource>,
    score: i32,
) -> SearchResult {
    let offered = OfferedAction {
        package_id: entry.id.clone(),
        display_name: entry.name.clone(),
        resource: action.resource.clone(),
        action: action.action.clone(),
    };
    SearchResult::nullary_fallible(
        ResourceKind::Action,
        PROVIDER_ID,
        action_row_id(&entry.id, &action.resource, &action.action),
        action.label.clone(),
        Some(entry.name.clone()),
        score,
        move |_doc, _root, chrome: &Chrome| run_offered_action(chrome, sessions.as_ref(), &offered),
    )
}

/// The active workspace's room and launch grants, captured now: a download
/// takes seconds, and the app lands where it was asked for.
fn launch_context(chrome: &Chrome) -> Option<(crate::shell::state::RoomId, LaunchGrants)> {
    let st = chrome.borrow();
    let tree = st.tree();
    tree.desktop
        .active_workspace()
        .map(|ws| (ws.room, ws.launch_grants(tilo_clock::unix_now())))
}

type LaunchGrants = collab_types::caps::LaunchGrants;

/// No active workspace is no room to open in: said the way an installed
/// app's launch says it, and returned to a caller that cannot see it.
fn report_no_workspace(chrome: &Chrome, display_name: &str) -> ActionError {
    let error = launcher::LaunchError::NoWorkspace;
    let mut st = chrome.borrow_mut();
    st.push_notification_full(
        Notification::new(
            launcher::launch_failure_title(display_name),
            error.notification_message(),
        )
        .with_severity(NotificationSeverity::Error)
        .with_app_name(t("host-source-applications"))
        .with_dismiss_after(launcher::LAUNCH_FAILURE_NOTICE_LIFETIME),
    );
    st.bump();
    ActionError::Refused(format!(
        "Couldn't open {display_name}: {}",
        error.user_message()
    ))
}

/// Open the Tilo app `package_id`: it downloads first when it is not here,
/// with no install step, and opens where it was asked for.
fn open_tilo_app(chrome: &Chrome, package_id: &str, display_name: &str) -> Result<(), ActionError> {
    // One activation owns the download; another press is the same request.
    if catalogue::pending_launch(package_id) {
        return Ok(());
    }
    // It arrived since this row was listed: open what is here.
    let installed = launcher::installed_apps()
        .is_ok_and(|apps| apps.iter().any(|app| app.package_id == package_id));
    if installed {
        return super::applications::launch_installed_app(chrome, package_id, display_name);
    }
    let Some((room, capabilities)) = launch_context(chrome) else {
        return Err(report_no_workspace(chrome, display_name));
    };
    catalogue::note_pending_launch(package_id);
    {
        // The opening card holds its place from this frame and shows the
        // download; the window binds into it once the app starts.
        let st = chrome.borrow();
        let placement = crate::shell::placement::placement_for_target(
            &st.tree(),
            st.strip_viewport(),
            st.open_target,
        );
        let _held = crate::shell::tab_binding::hold_place_for_launch(
            &mut st.tree_mut(),
            package_id,
            display_name,
            placement,
        );
    }
    // The window's deadline runs from when the app can start, not from the
    // press: a cold download can take most of it.
    launcher::hold_presentation_for(package_id, catalogue::INSTALL_TIMEOUT);
    catalogue::install_and_launch(
        package_id.to_owned(),
        display_name.to_owned(),
        room,
        capabilities,
    );
    Ok(())
}

/// Install the Flathub app `app_id`, then open it.
fn install_flathub_app(
    chrome: &Chrome,
    app_id: &str,
    display_name: &str,
) -> Result<(), ActionError> {
    if !app_catalogue::flathub::valid_app_id(app_id) {
        return Err(ActionError::Refused(t("catalogue-error-flathub-invalid")));
    }
    if catalogue::installing_from_flathub(app_id) {
        return Ok(());
    }
    let (placement, portal_workspace_id) = {
        let st = chrome.borrow();
        let tree = st.tree();
        // Decided now, while the layout is still true; the window maps
        // after the install, keyed on the app id it will announce.
        let placement = crate::shell::placement::note_placement_for_launch_identities(
            app_id,
            &[app_id.to_owned()],
            &tree,
            st.strip_viewport(),
            st.open_target,
        );
        let workspace = tree
            .desktop
            .active_workspace()
            .map(|workspace| workspace.room.to_string());
        (placement, workspace)
    };
    {
        let mut st = chrome.borrow_mut();
        st.push_notification_full(
            Notification::new(
                t_args(
                    "search-catalogue-installing",
                    &[("app", display_name.into())],
                ),
                t("search-catalogue-installing-body"),
            )
            .with_app_name(t("host-source-applications")),
        );
        st.bump();
    }
    catalogue::install_flathub_and_open(
        app_id.to_owned(),
        display_name.to_owned(),
        placement,
        portal_workspace_id,
    );
    Ok(())
}

/// The App Store's page for `app_id`, where it can be reviewed, blocked or
/// installed by hand.
fn store_detail_uri(app_id: &str) -> Result<String, ActionError> {
    if app_id.is_empty()
        || app_id.len() > 255
        || app_id
            .chars()
            .any(|ch| !ch.is_ascii_alphanumeric() && !matches!(ch, '.' | '-' | '_'))
    {
        return Err(ActionError::Refused(
            "The catalogue entry has an invalid stable package identity".into(),
        ));
    }
    Ok(format!("tilo-store:app/{app_id}"))
}

/// Open the App Store at `app_id`'s page.
fn show_in_store(chrome: &Chrome, app_id: &str, display_name: &str) -> Result<(), ActionError> {
    let uri = store_detail_uri(app_id)?;
    let Some((room, capabilities)) = launch_context(chrome) else {
        return Err(report_no_workspace(chrome, display_name));
    };
    launcher::launch_uri_in_room(STORE_PACKAGE_ID, room, capabilities, &uri)
        .map(|_| ())
        .map_err(|error| {
            let message = error.user_message();
            let mut state = chrome.borrow_mut();
            state.push_notification_full(
                Notification::new(
                    t_args(
                        "search-catalogue-store-failed",
                        &[("app", display_name.into())],
                    ),
                    message.clone(),
                )
                .with_severity(NotificationSeverity::Error)
                .with_app_name(t("host-source-applications")),
            );
            state.bump();
            ActionError::Refused(format!(
                "Couldn't open the App Store for {display_name}: {message}"
            ))
        })
}

/// An action a Tilo app offers while closed, on an app not on this device.
#[derive(Clone)]
struct OfferedAction {
    package_id: String,
    display_name: String,
    resource: String,
    action: String,
}

/// Get the app if needed, then run its offered action through the session's
/// action host, as the person in the active workspace. The host starts the
/// app and the action it publishes admits the invocation, exactly as for an
/// installed app that is closed.
fn run_offered_action(
    chrome: &Chrome,
    sessions: Option<&ActionSessionSource>,
    offered: &OfferedAction,
) -> Result<(), ActionError> {
    let Some(sessions) = sessions.cloned() else {
        return Err(ActionError::Refused(t(
            "search-catalogue-actions-unavailable",
        )));
    };
    let room = {
        let st = chrome.borrow();
        let tree = st.tree();
        tree.desktop
            .active_workspace()
            .map(|workspace| workspace.room)
    };
    let Some(room) = room else {
        return Err(report_no_workspace(chrome, &offered.display_name));
    };
    let person = chrome.borrow().local_user_for_room(&room);
    let room = room.to_string();
    let OfferedAction {
        package_id,
        display_name,
        resource,
        action,
    } = offered.clone();
    let installed = launcher::installed_apps()
        .is_ok_and(|apps| apps.iter().any(|app| app.package_id == package_id));
    if installed {
        invoke_offered(
            &sessions,
            &room,
            &person,
            &package_id,
            &display_name,
            (&resource, &action),
        );
        return Ok(());
    }
    catalogue::install_then(package_id, display_name, move |package_id, display_name| {
        invoke_offered(
            &sessions,
            &room,
            &person,
            package_id,
            display_name,
            (&resource, &action),
        );
    });
    Ok(())
}

/// Invoke `resource`'s `action` of `package_id` through the action host. A
/// refusal is reported as the app's failure to open, since that is what the
/// person sees not happen.
fn invoke_offered(
    sessions: &ActionSessionSource,
    room: &str,
    person: &str,
    package_id: &str,
    display_name: &str,
    (resource, action): (&str, &str),
) {
    let session = match sessions.open(room, Some(person)) {
        Ok(session) => session,
        Err(error) => {
            catalogue::report_install_failure(package_id, display_name, error);
            return;
        }
    };
    let pending = session.invoke_job(tilo_actions::remote::InvokeRequest {
        on_behalf_of: None,
        invocation: tilo_actions::InvocationId::new(),
        target: tilo_actions::remote::application_action_target(room, package_id, resource, action),
        argument: tilo_actions::remote::InvokeArgument::None,
        context: None,
        confirmation: None,
        revision: None,
        wait_ms: None,
        appearance: None,
    });
    let (package_id, display_name) = (package_id.to_owned(), display_name.to_owned());
    pending.then(move |answer| {
        if let Some(Err(error)) = answer {
            catalogue::report_install_failure(&package_id, &display_name, error.to_string());
        }
    });
}

impl SearchProvider for StoreCatalogueProvider {
    fn id(&self) -> &'static str {
        PROVIDER_ID
    }

    fn name(&self) -> &'static str {
        "App Store"
    }

    fn display_name(&self) -> String {
        t("search-catalogue-provider")
    }

    fn resource_kinds(&self) -> &[ResourceKind] {
        &self.kinds
    }

    fn query(&self, ctx: SearchContext<'_>) -> Vec<SearchResult> {
        let _ = self.search_revision(&ctx);
        self.query_page(ctx, None, SearchBudget::PALETTE).results
    }

    fn row_icon(&self, result: &SearchResult) -> Option<ui::icons::Icon> {
        let app = app_of_row(&result.id)?;
        let category = self.snapshot().and_then(|snapshot| {
            snapshot
                .entries
                .iter()
                .find(|entry| entry.id == app)
                .and_then(|entry| entry.category.clone())
        });
        Some(ui::icons::app_glyph(app, category.as_deref()))
    }

    fn row_image(&self, result: &SearchResult) -> Option<ui::icons::AppImage> {
        let snapshot = self.snapshot()?;
        let path = snapshot.icons.get(&result.id)?;
        crate::app_icons::themed(path.to_str()?)
    }

    fn secondary_actions(&self, result: &SearchResult) -> Vec<SearchResult> {
        let Some(app_id) = result.id.strip_prefix(FLATHUB_PREFIX) else {
            return Vec::new();
        };
        let app_id = app_id.to_owned();
        let name = result.title.clone();
        vec![SearchResult::nullary_fallible(
            ResourceKind::Action,
            PROVIDER_ID,
            format!("{STORE_PAGE_PREFIX}{app_id}"),
            t("search-catalogue-show-in-store"),
            None,
            result.score,
            move |_doc, _root, chrome: &Chrome| show_in_store(chrome, &app_id, &name),
        )]
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
        // Flathub's metadata and the block list are looked at again when
        // that is due, on a worker; a test reads only its injected source.
        #[cfg(not(test))]
        catalogue::refresh_shared();
        self.index
            .refresh(|| self.source_generation(), self.builder());
    }

    fn index_settling(&self, _ctx: &SearchContext<'_>) -> bool {
        self.snapshot().is_none()
    }

    fn search_revision(&self, _ctx: &SearchContext<'_>) -> String {
        self.revision()
    }

    fn resolve(&self, id: &str, ctx: SearchContext<'_>) -> Option<SearchResult> {
        if !ctx.wanted.iter().any(|kind| self.kinds.contains(kind)) {
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
        let row = snapshot
            .rows
            .iter()
            .copied()
            .find(|row| snapshot.row_id(*row).as_deref() == Some(id))?;
        self.row(&snapshot, row, provider_score(0))
    }

    fn query_page(
        &self,
        ctx: SearchContext<'_>,
        cursor: Option<&SearchCursor>,
        budget: SearchBudget,
    ) -> SearchPage {
        let revision = self.revision();
        // Nothing to get is asked for by an empty query: the catalogue is
        // thousands of apps, browsed in the App Store, not in search.
        if ctx.needle.trim().is_empty()
            || !ctx.wanted.iter().any(|kind| self.kinds.contains(kind))
            || budget.max_results == 0
            || budget.max_work == 0
        {
            return SearchPage::exhausted(revision, Vec::new());
        }
        let Some(snapshot) = self.snapshot() else {
            return SearchPage::exhausted(revision, Vec::new());
        };
        crate::shell::search::index_page(
            revision,
            cursor,
            ctx.needle,
            budget,
            |page| snapshot.index.search(ctx.needle, page),
            |hit| {
                let row = usize::try_from(hit.key)
                    .ok()
                    .and_then(|at| snapshot.rows.get(at))
                    .copied()?;
                let kind = match row {
                    Row::App(_) => ResourceKind::Application,
                    Row::Document { .. } | Row::Action { .. } => ResourceKind::Action,
                };
                if !ctx.wanted.contains(&kind) {
                    return None;
                }
                self.row(&snapshot, row, provider_score(hit.rank.unwrap_or(0)))
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::chrome_state::ChromeState;
    use crate::shell::state::WorkspaceTree;
    use automerge::AutoCommit;

    /// Flathub's cached metadata for one app, as Flatpak keeps it.
    const SPOTIFY: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<components version="0.16" origin="flathub">
  <component type="desktop-application">
    <id>com.spotify.Client</id>
    <name>Spotify</name>
    <summary>Online music streaming service</summary>
    <icon type="cached" height="128" width="128">com.spotify.Client.png</icon>
    <categories><category>Audio</category></categories>
    <keywords><keyword>music</keyword></keywords>
    <bundle type="flatpak">app/com.spotify.Client/x86_64/stable</bundle>
  </component>
</components>"#;

    fn flathub() -> Vec<CatalogueEntry> {
        let source = app_catalogue::appstream::AppstreamSource {
            remote: "flathub".into(),
            installation: app_catalogue::Installation::System,
            file: PathBuf::from("/var/lib/flatpak/appstream/flathub/x86_64/active/appstream.xml"),
            icons: PathBuf::from("/var/lib/flatpak/appstream/flathub/x86_64/active/icons"),
        };
        app_catalogue::appstream::parse(SPOTIFY.as_bytes(), &source, "en").expect("parses")
    }

    fn offered(package_id: &str, display_name: &str) -> CatalogueApp {
        CatalogueApp {
            package_id: package_id.into(),
            display_name: display_name.into(),
            version: "1.0.0".into(),
            registry: "Tilo".into(),
            trust: "publisher-signed".into(),
            ..CatalogueApp::default()
        }
    }

    fn state(installed: &[&str], blocked: &[&str]) -> StoreCatalogueTestState {
        StoreCatalogueTestState {
            installed: installed.iter().map(|id| (*id).to_owned()).collect(),
            tilo: vec![
                offered("so.tilo.calendar", "Calendar"),
                offered("so.tilo.sheets", "Sheets"),
            ],
            shared: SharedCatalogue {
                flathub: std::sync::Arc::new(flathub()),
                blocks: std::sync::Arc::new(
                    app_catalogue::BlockList::empty()
                        .with_workspace_policy("School", blocked.iter().copied()),
                ),
            },
        }
    }

    fn search(provider: &StoreCatalogueProvider, needle: &str) -> Vec<SearchResult> {
        let chrome = ChromeState::new(WorkspaceTree::bootstrap_default());
        let state = chrome.borrow();
        let doc = AutoCommit::new();
        let ctx = SearchContext {
            query_scope: None,
            include_programmatic: false,
            needle,
            wanted: &[ResourceKind::Application, ResourceKind::Action],
            chrome: &state,
            doc: &doc,
            doc_root: &automerge::ROOT,
        };
        let deadline = tilo_test_env::hang_deadline();
        while provider.snapshot().is_none() {
            provider.refresh_search_index(&ctx);
            assert!(
                std::time::Instant::now() < deadline,
                "the catalogue index did not become ready"
            );
            std::thread::yield_now();
        }
        provider.query(ctx)
    }

    // USER REQUIREMENT (2026-10-19): "i cant search spotify, youtube, my
    // unlocked passwords, the app store for flatpaks – i cannot search
    // ANYTHING."
    #[test]
    fn spotify_from_the_cached_flathub_catalogue_is_found_and_installs() {
        let provider = StoreCatalogueProvider::with_test_source(StoreCatalogueTestSource::new(
            state(&[], &[]),
        ));
        let rows = search(&provider, "spotify");
        let spotify = rows
            .iter()
            .find(|row| row.id == "flathub:com.spotify.Client")
            .expect("\"spotify\" finds Spotify from Flathub");
        assert_eq!(spotify.title, "Spotify");
        assert_eq!(spotify.kind, ResourceKind::Application);
        assert_eq!(
            spotify.subtitle.as_deref(),
            Some("Online music streaming service"),
            "the tag line is the second line, a text run of its own"
        );
        assert_eq!(
            spotify.badge.as_deref(),
            Some("Flathub"),
            "the row says where it comes from"
        );
        assert!(
            spotify.score < 100,
            "an app to install ranks below an installed app of the same match"
        );
        let store = provider.secondary_actions(spotify);
        assert_eq!(store.len(), 1, "its other action shows it in the Store");
        assert_eq!(store[0].id, "store-page:com.spotify.Client");
        let snapshot = provider.snapshot().expect("indexed");
        assert_eq!(
            snapshot.icons.get(&spotify.id),
            Some(&PathBuf::from(
                "/var/lib/flatpak/appstream/flathub/x86_64/active/icons/128x128/com.spotify.Client.png"
            )),
            "the row wears the app's own cached icon"
        );
    }

    // USER REQUIREMENT (2026-10-19): "directly opening tapps without explicit
    // installation" — and an app this device has is never offered again.
    #[test]
    fn installed_apps_are_never_offered_and_tilo_apps_just_open() {
        let source = StoreCatalogueTestSource::new(state(&[], &[]));
        let provider = StoreCatalogueProvider::with_test_source(Arc::clone(&source));
        let sheets = search(&provider, "sheets");
        let row = sheets
            .iter()
            .find(|row| row.id == "app:so.tilo.sheets")
            .expect("an uninstalled Tilo app is found");
        assert_eq!(
            row.title, "Sheets",
            "it is named as the app, not as a Store page"
        );
        assert!(row.badge.is_none(), "a Tilo app has nothing to install");

        source.replace(state(
            &["so.tilo.sheets", "com.spotify.Client.desktop"],
            &[],
        ));
        assert!(
            search(&provider, "sheets")
                .iter()
                .all(|row| !row.id.ends_with("so.tilo.sheets")
                    && !row.id.contains("so.tilo.sheets/")),
            "an installed Tilo app has no installable row, nor its documents"
        );
        assert!(
            search(&provider, "spotify")
                .iter()
                .all(|row| row.id != "flathub:com.spotify.Client"),
            "an installed Flatpak has no installable row"
        );
    }

    // USER REQUIREMENT (2026-10-19): "apps can of course be blocked in the
    // app store"
    #[test]
    fn blocked_apps_appear_nowhere_in_search() {
        let provider = StoreCatalogueProvider::with_test_source(StoreCatalogueTestSource::new(
            state(&[], &["com.spotify.Client", "so.tilo.calendar"]),
        ));
        assert!(
            search(&provider, "spotify")
                .iter()
                .all(|row| row.id != "flathub:com.spotify.Client")
        );
        assert!(
            search(&provider, "show tasks")
                .iter()
                .chain(search(&provider, "calendar").iter())
                .all(|row| app_of_row(&row.id) != Some("so.tilo.calendar")),
            "neither a blocked app nor its actions"
        );
    }

    // USER REQUIREMENT (2026-10-19): "all resource types and actions must
    // also be searchable, even if app not installed"
    #[test]
    fn an_uninstalled_tilo_apps_documents_and_actions_are_found() {
        let provider = StoreCatalogueProvider::with_test_source(StoreCatalogueTestSource::new(
            state(&[], &[]),
        ));
        let tasks = search(&provider, "show tasks");
        let action = tasks
            .iter()
            .find(|row| row.id == "offer:so.tilo.calendar/calendar.calendar/show-tasks")
            .expect("Calendar's closed action is found while Calendar is not here");
        assert_eq!(action.title, "Show tasks");
        assert_eq!(action.subtitle.as_deref(), Some("Calendar"));
        assert_eq!(action.kind, ResourceKind::Action);

        let workbook = search(&provider, "workbook");
        let document = workbook
            .iter()
            .find(|row| row.id == "new:so.tilo.sheets/sheets.workbook")
            .expect("Sheets' document type is found while Sheets is not here");
        assert_eq!(document.title, "Workbook");
        assert_eq!(document.subtitle.as_deref(), Some("New in Sheets"));
        assert_eq!(
            provider.row_icon(document),
            Some(ui::icons::app_glyph("so.tilo.sheets", None)),
            "it wears its app's symbol"
        );
    }

    #[test]
    fn an_empty_query_offers_nothing_to_get() {
        let provider = StoreCatalogueProvider::with_test_source(StoreCatalogueTestSource::new(
            state(&[], &[]),
        ));
        assert!(search(&provider, "").is_empty());
    }
}
