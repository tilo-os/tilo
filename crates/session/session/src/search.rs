//! The desktop's search in the native session: the shared desktop search ([`tilo_desktop_search`])
//! the desk opens, over what is published and what the session's services answer, each row run by
//! its address.
//! The desk lists the windows, the people and the desktop's own commands. The session adds what
//! only a device has: the apps installed here and the machine's own (with the person's switch),
//! Settings' pages, the store's catalogue, what apps hold (the content index), the person's files,
//! where their windows were, the actions open apps publish through the action host and the jobs it
//! runs, and the session's own published actions. A chosen row the session carries out: its own
//! action through its own published surface, an app's through the action host by address (the app
//! started first when it is closed), an app's start, an address.

#![cfg_attr(
    target_arch = "wasm32",
    allow(
        dead_code,
        reason = "the page hosts this search over the desk it runs next"
    )
)]

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use crate::platform::apps::Settings;
use crate::platform::apps::{Apps, Reading};
use crate::platform::indexes::Indexes;
use crate::records::Records;

use tilo_actions::DoorError;
use tilo_actions::remote::downloaded_apps as downloaded_wire;
use tilo_actions::remote::{
    ActionHostError, Caller, Found, RemoteActionTarget, ResourceSearch, application_action_target,
    application_open_target,
};
use tilo_desktop_search::preferences;
use tilo_desktop_search::providers::PublishedActions;
use tilo_desktop_search::providers::downloaded::Downloaded;
use tilo_desktop_search::providers::happening::{Happening, WorkspaceHere};
use tilo_desktop_search::providers::host_apps;
use tilo_desktop_search::providers::jobs::{JobEntry, Jobs};
use tilo_desktop_search::providers::places::{Continue, Place, RecentPlaces};
use tilo_desktop_search::providers::records as record_rows;
use tilo_desktop_search::providers::waiting::Waiting;
use tilo_desktop_search::{
    DESKTOP_APP, Held, InvokeTarget, LaunchPlace, SearchPreferences, Source, Target, Usage,
};
use tilo_desktop_surface::{SearchHost, SearchHostFeed};
use ui::record::{RecordCheckout, RecordId, RecordKind};
use ui::search::Scope;

/// The action host as the session's search reaches it: what open apps publish and run, and running
/// what is chosen by address.
pub trait AppActions {
    /// What the apps publish through the action host, by app and workspace: those open now and
    /// those that closed after publishing, which a chosen row starts again. `workspace` narrows it
    /// to one workspace, and the empty string asks for every workspace's.
    fn published(&self, workspace: &str) -> Vec<PublishedActions>;
    /// The invocations the action host follows for the person.
    fn jobs(&self) -> Vec<JobEntry>;
    /// Run the action `target` names with `arguments`; an app's own action
    /// starts the app first when it is closed, the workspace's `open` of an
    /// app starts it.
    ///
    /// # Errors
    /// The host's refusal.
    fn invoke(
        &self,
        target: RemoteActionTarget,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), DoorError>;
    /// Ask the invocation `invocation` of `app` in `workspace` to stop.
    ///
    /// # Errors
    /// The host's refusal: no such invocation, or it cannot be stopped.
    fn cancel(&self, app: &str, workspace: &str, invocation: &str) -> Result<(), DoorError>;
    /// Open `uri` in `workspace`, in the app that takes its scheme: a file,
    /// a Store page, a web address.
    ///
    /// # Errors
    /// Nothing takes it.
    fn open_uri(&self, workspace: &str, uri: &str) -> Result<(), DoorError>;
    /// Start the machine's own app `desktop_id`.
    ///
    /// # Errors
    /// It did not start.
    fn start_host_app(&self, desktop_id: &str) -> Result<(), DoorError>;
    /// Open the application `app` in `workspace`, where the person asked it:
    /// a window of its own beside theirs when `place` says so, else the
    /// window the app has there. A host that starts no windows arranges
    /// nothing, and opens the app where it already opens.
    ///
    /// # Errors
    /// The host's refusal.
    fn open_application(
        &self,
        workspace: &str,
        app: &str,
        place: LaunchPlace,
    ) -> Result<(), DoorError> {
        let _ = place;
        self.invoke(
            application_open_target(workspace, app),
            serde_json::Map::new(),
        )
    }
}

/// What the session's search reaches beyond the desktop.
#[derive(Default)]
pub struct SearchOptions {
    /// What apps hold and what the person's files say, as this host indexes them; `None` where the
    /// host keeps no index.
    pub indexes: Option<Arc<dyn Indexes>>,
    /// The settings this host offers search, where it has any.
    pub settings: Option<Arc<dyn Settings>>,
    /// The sources the person switched off.
    pub preferences: SearchPreferences,
    /// Where the person's windows were.
    pub places: RecentPlaces,
    /// The records the workspaces on this device hold: what search finds by name across them, and
    /// where a record brought here is named.
    pub records: Option<Records>,
}

/// The sources the session keeps between searches, each indexed once per reading of the apps they
/// are built from.
struct Kept {
    /// The reading of the host's apps the sources below were built from.
    reading: Option<Arc<dyn Reading>>,
    host_apps: Option<Held>,
    content: Option<Held>,
    files: Option<Held>,
    /// Settings' pages and the store's catalogue, for the workspace they were built for.
    workspace: Option<(String, Held, Held)>,
}

/// The programs the person downloaded, as the registry last read them, and the session's published
/// actions for them: the apps, the offers, and what a row whose file is gone runs.
#[derive(Default)]
struct DownloadedRows {
    reading: downloaded_wire::DownloadedApps,
    published: Vec<tilo_actions::ActionDescriptor>,
}

/// The session's part in the desk's search.
pub(crate) struct SessionSearch {
    /// The apps this host lists, where it lists any.
    apps_lists: Option<Arc<dyn Apps>>,
    /// The model gateway this session's own questions run on, where it runs one.
    models: Option<Arc<dyn crate::platform::Models>>,
    /// What apps hold and what the person's files say, as this session reads them.
    indexes: Option<Arc<dyn crate::platform::indexes::Index>>,
    /// The settings this host offers search.
    settings: Option<Arc<dyn Settings>>,
    /// The sources the person switched off, as kept: what the next search leaves out, and what a
    /// switch from search changes.
    preferences: RefCell<SearchPreferences>,
    places: RefCell<RecentPlaces>,
    /// What the person chose before; `None` until they chose or it was restored.
    usage: RefCell<Option<Usage>>,
    /// The parts of the history changed since they were last kept: the choices, the places.
    unkept: Cell<(bool, bool)>,
    kept: RefCell<Kept>,
    apps: Option<Rc<dyn AppActions>>,
    /// The records the workspaces on this device hold.
    records: Option<Records>,
    /// What the session itself publishes, as of the last frame: the desk cannot read its own tree
    /// while it opens search.
    own: RefCell<Vec<tilo_actions::ActionDescriptor>>,
    /// What the resources the session publishes are, by id, as of the last frame: what a query
    /// naming a type ("my mails") finds them by.
    own_types: RefCell<std::collections::BTreeMap<String, tilo_actions::ResourceType>>,
    /// The windows open, as of the last frame.
    open_tabs: RefCell<Vec<u64>>,
    /// The live notifications the session publishes, as of the last frame.
    notifications: RefCell<Vec<tilo_actions::ResourceSpec>>,
    /// The workspaces the desk shows, by id and the name it gives them: what a row of another
    /// workspace says.
    workspaces: RefCell<Vec<(String, String)>>,
    /// Who is in each workspace and the calls in progress, as the desk and the presence it holds
    /// say: what the empty palette's first group lists.
    here: RefCell<Vec<WorkspaceHere>>,
    /// The downloaded programs, as of the last reading of the registry.
    downloaded: RefCell<DownloadedRows>,
    /// The question search answered, and the answer as it is being written: kept across the builds
    /// of the rows so what arrives stays where the person asked it.
    ask: crate::search_ask::Shared,
    /// What wakes the session's loop as an answer is written, so the row shows it as it lands.
    wake: Option<tilo_service::Wake>,
}

/// What an app is called here, as the host's own lists name it: the answer a row says an app by.
fn names(apps: Option<Arc<dyn Apps>>) -> impl Fn(&str) -> String + Send + Sync + 'static {
    move |app: &str| match &apps {
        Some(apps) => apps.name(app),
        None => tilo_desktop_search::names::app_name(app),
    }
}

/// The resource of the section keeping search's history.
pub(crate) const HISTORY: &str = "settings:settings.search_history";

/// The field edit setting the history's field `field` to `value` .
fn set_field(field: &str, value: &serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
    let mut edit = serde_json::Map::new();
    edit.insert(
        "edit".into(),
        serde_json::json!({ "path": [{ "field": field }], "op": "set", "value": value }),
    );
    edit
}

impl SessionSearch {
    pub(crate) fn new(
        options: SearchOptions,
        lists: Option<Arc<dyn Apps>>,
        person: &str,
        apps: Option<Rc<dyn AppActions>>,
        wake: Option<tilo_service::Wake>,
        models: Option<Arc<dyn crate::platform::Models>>,
    ) -> Self {
        let SearchOptions {
            indexes,
            settings,
            preferences,
            places,
            records,
        } = options;
        let reading = lists.as_ref().map(|lists| lists.reading());
        let indexed = indexes.as_ref().map(|indexes| indexes.of(person));
        let kept = Kept {
            host_apps: reading.as_ref().map(|read| read.host_apps(&preferences)),
            reading,
            content: indexed.as_ref().map(|index| index.content()),
            files: indexed.as_ref().and_then(|index| index.files()),
            workspace: None,
        };
        Self {
            apps_lists: lists,
            preferences: RefCell::new(preferences),
            places: RefCell::new(places),
            usage: RefCell::new(None),
            unkept: Cell::new((false, false)),
            kept: RefCell::new(kept),
            models,
            indexes: indexed,
            settings,
            apps,
            records,
            own: RefCell::default(),
            own_types: RefCell::default(),
            open_tabs: RefCell::default(),
            notifications: RefCell::default(),
            workspaces: RefCell::default(),
            here: RefCell::default(),
            downloaded: RefCell::default(),
            ask: crate::search_ask::Shared::default(),
            wake,
        }
    }

    /// What the session publishes now, the windows open and the downloaded programs as the registry
    /// reads them: read after each frame, outside the desktop's trees.
    pub(crate) fn observe(
        &self,
        own: &[tilo_actions::ResourceSpec],
        open_tabs: Vec<u64>,
        downloaded: Option<downloaded_wire::DownloadedApps>,
        notifications: Vec<tilo_actions::ResourceSpec>,
    ) {
        *self.notifications.borrow_mut() = notifications;
        let mut published = tilo_actions::door::survey(own);
        // Search does not list itself, nor opening itself.
        published.retain(|action| {
            let address = &action.address;
            let searching = address
                .resource
                .starts_with(tilo_desktop::desk_verbs::SEARCH_RESOURCE);
            let opening = address.resource == tilo_desktop::desk_verbs::DESK_RESOURCE
                && address.action == "search";
            !searching && !opening
        });
        // The desk's own resources are named after the person and the desktop, not after the thing
        // an action acts on: matched as a word, that name would list every one of the desk's
        // actions for a query that names the person.
        for action in &mut published {
            if matches!(
                action.address.resource.as_str(),
                tilo_actions::remote::session_actions::DESK_RESOURCE
                    | tilo_desktop_search::CHROME_RESOURCE
            ) {
                action.resource_label.clear();
            }
        }
        *self.own.borrow_mut() = published;
        *self.own_types.borrow_mut() = tilo_desktop_search::providers::types_of(own);
        *self.open_tabs.borrow_mut() = open_tabs;
        if let Some(reading) = downloaded {
            let published = self
                .own
                .borrow()
                .iter()
                .filter(|action| {
                    let resource = action.address.resource.as_str();
                    resource == downloaded_wire::RESOURCE
                        || resource.starts_with(downloaded_wire::APP_PREFIX)
                })
                .cloned()
                .collect();
            *self.downloaded.borrow_mut() = DownloadedRows { reading, published };
        }
    }

    /// Who is in the person's workspaces now, as their presence and the desk's collaborators say
    /// it: the empty palette's first group.
    pub(crate) fn set_here(&self, workspaces: Vec<WorkspaceHere>) {
        *self.here.borrow_mut() = workspaces;
    }

    /// The workspaces this device shows, by id and the name the desk gives it: what a row says of a
    /// record the workspace in front does not hold.
    pub(crate) fn set_workspaces(&self, workspaces: Vec<(String, String)>) {
        if let Some(records) = &self.records {
            records.set_workspaces(workspaces.clone());
        }
        *self.workspaces.borrow_mut() = workspaces;
    }

    /// Search `source` from now on, or stop: the person's choice, kept in the settings' `search`
    /// section so it holds whichever desktop searches and outlasts this session.
    fn prefer(&self, source: &str, enabled: bool) {
        let mut preferences = self.preferences.borrow_mut();
        if preferences.enabled(source) == enabled {
            return;
        }
        preferences.set_enabled(source, enabled);
        let next = preferences.clone();
        drop(preferences);
        // The machine's apps leave out what is switched off as they are built, so the search after
        // this one reads that list again.
        let reading = self.apps_lists.as_ref().map(|lists| lists.reading());
        let host_apps = reading.as_ref().map(|read| read.host_apps(&next));
        {
            let mut kept = self.kept.borrow_mut();
            kept.host_apps = host_apps;
            kept.reading = reading;
            kept.workspace = None;
        }
        if let Err(error) = settings_store::update_section(preferences::SECTION, next.to_values()) {
            tracing::warn!(%error, "the search sources switched off are not kept");
        }
    }

    /// The sources a search asks over `workspace`, and what this device's action host publishes
    /// there: one list, so a tool's search finds what the palette finds.
    fn sources(&self, workspace: &str, scope: &Scope) -> (Vec<Held>, Vec<PublishedActions>) {
        let now = ui::time::unix_now();
        let (settings, catalogue) = self.for_workspace(workspace);
        let kept = self.kept.borrow();
        let names = names(self.apps_lists.clone());
        // Every workspace's places name their workspace when the search is widened; the one in
        // front needs no name.
        let continue_places = match scope {
            Scope::Everywhere => Continue::everywhere(
                &self.places.borrow(),
                workspace,
                self.open_tabs.borrow().clone(),
                self.workspaces.borrow().clone(),
                now,
                names,
            ),
            _ => Continue::new(
                &self.places.borrow(),
                workspace,
                self.open_tabs.borrow().clone(),
                now,
                names,
            ),
        };
        let mut sources: Vec<Held> = vec![
            Held::new(Happening::new(
                scope.everywhere(),
                self.here.borrow().clone(),
            )),
            Held::new(Waiting::new(workspace, self.notifications.borrow().clone())),
            Held::new(continue_places),
        ];
        sources.extend(settings);
        sources.extend(catalogue);
        sources.extend(kept.host_apps.clone());
        {
            let downloaded = self.downloaded.borrow();
            sources.push(Held::new(Downloaded::new(
                workspace,
                downloaded.reading.clone(),
                &downloaded.published,
            )));
        }
        sources.extend(kept.content.clone());
        sources.extend(kept.files.clone());
        if let Some(records) = &self.records {
            sources.push(Held::new(record_rows::Records::new(
                workspace,
                records.held(workspace),
            )));
        }
        let mut published = vec![PublishedActions {
            app: DESKTOP_APP.into(),
            workspace: workspace.to_owned(),
            actions: self.own.borrow().clone(),
            types: self.own_types.borrow().clone(),
        }];
        if let Some(apps) = &self.apps {
            sources.push(Held::new(Jobs(apps.jobs())));
            // A search over one workspace offers that workspace's actions;
            // one that reaches everywhere offers every workspace's.
            published.extend(apps.published(if scope.everywhere() { "" } else { workspace }));
        }
        (sources, published)
    }

    /// The results of one search over the sources a query that names a resource asks, in the
    /// ranking's order: what text typed on the canvas places (shell.md §8). Every one, as ranked:
    /// the best resource to place may stand below many actions the text names ("Inbox" under each
    /// conversation's "Move to inbox").
    #[must_use]
    pub(crate) fn find(&self, needle: &str) -> Vec<tilo_desktop_search::Hit> {
        self.find_in(needle, None)
    }

    /// [`Self::find`] for a caller confined to `workspace` (an agent): what that workspace's apps
    /// publish and its records hold, and none of the desk's own rows, whose windows and places
    /// are the person's across every workspace (a window's title names what another workspace
    /// holds). `None` is the person's search, over everything they see.
    #[must_use]
    pub(crate) fn find_in(
        &self,
        needle: &str,
        workspace: Option<&str>,
    ) -> Vec<tilo_desktop_search::Hit> {
        let needle = needle.trim();
        if needle.is_empty() {
            return Vec::new();
        }
        let (mut sources, mut published) = self.sources(workspace.unwrap_or(""), &Scope::Workspace);
        if workspace.is_some() {
            published.retain(|actions| actions.app != DESKTOP_APP);
        }
        sources.push(Held::new(tilo_desktop_search::providers::Captures(
            published.clone(),
        )));
        sources.push(Held::new(tilo_desktop_search::providers::Published(
            published,
        )));
        let refs: Vec<&dyn Source> = sources.iter().map(|source| source as &dyn Source).collect();
        // The search over everything but the answers a query that is not a name gets (a sum, a web
        // search): text typed on the canvas names a resource to place, never a sentence to search
        // the web for. The one ranking, with what the person chose before: a tool that places text
        // on the canvas and the palette lead with the same documents, apps and actions in the same
        // order.
        let now = ui::time::unix_now();
        let usage = self.usage.borrow();
        let groups = tilo_desktop_search::search(needle, &refs, 0, &|hit| match usage.as_ref() {
            Some(usage) => usage.strength(needle, hit, now),
            None => 0.0,
        });
        groups.into_iter().flat_map(|group| group.results).collect()
    }

    /// A window was at `place` .
    pub(crate) fn record_place(&self, place: Place) {
        let mut places = self.places.borrow_mut();
        let before = places.places().first().cloned();
        places.record(place);
        if places.places().first() != before.as_ref() {
            let (chosen, _) = self.unkept.get();
            self.unkept.set((chosen, true));
        }
    }

    /// Take up the history as the search history section ([`HISTORY`]) keeps it.
    pub(crate) fn restore(&self, kept: &serde_json::Value) {
        let text = |field: &str| kept.get(field).and_then(serde_json::Value::as_str);
        if let (Some(chosen), Some(for_text)) = (text("chosen"), text("chosen_for_text"))
            && !(chosen.is_empty() && for_text.is_empty())
        {
            *self.usage.borrow_mut() = Some(Usage::load(chosen, for_text));
        }
        if let Some(places) = kept
            .get("places")
            .filter(|places| !places.is_null())
            .and_then(|places| {
                serde_json::from_value(places.clone())
                    .inspect_err(
                        |error| tracing::warn!(%error, "the search places do not read back"),
                    )
                    .ok()
            })
        {
            *self.places.borrow_mut() = places;
        }
    }

    /// The field edits that keep what changed of the history since it was last kept, for the search
    /// history section's `field-edit` .
    pub(crate) fn take_unkept(&self) -> Vec<serde_json::Map<String, serde_json::Value>> {
        let (chosen, places) = self.unkept.replace((false, false));
        let mut edits = Vec::new();
        if chosen && let Some(usage) = self.usage.borrow().as_ref() {
            let (chosen, for_text) = usage.save();
            edits.push(set_field("chosen", &chosen.into()));
            edits.push(set_field("chosen_for_text", &for_text.into()));
        }
        if places {
            match serde_json::to_value(&*self.places.borrow()) {
                Ok(places) => edits.push(set_field("places", &places)),
                Err(error) => tracing::warn!(%error, "recent places not kept"),
            }
        }
        edits
    }

    /// The agents, people and conversations `query` names, as a window's box lists whom a message
    /// could go to.
    pub(crate) fn recipients(&self, query: &str) -> Vec<ui::floating_surface::Recipient> {
        self.indexes
            .as_ref()
            .map_or_else(Vec::new, |indexes| indexes.recipients(query))
    }

    /// The apps this session opens, for what it asks to be opened outside a search: a link a page
    /// names, a page a sheet asks for.
    pub(crate) fn apps(&self) -> Option<Rc<dyn AppActions>> {
        self.apps.clone()
    }

    /// The host's apps as last read, with the sources built from them rebuilt when they were read
    /// again since: the machine's apps here, Settings and the catalogue at their workspace's next
    /// search.
    fn current_reading(&self) -> Option<Arc<dyn Reading>> {
        let latest = self.apps_lists.as_ref().map(|lists| lists.reading());
        let mut kept = self.kept.borrow_mut();
        let same = match (&latest, &kept.reading) {
            (Some(latest), Some(kept)) => latest.generation() == kept.generation(),
            (None, None) => true,
            _ => false,
        };
        if !same {
            kept.host_apps = latest
                .as_ref()
                .map(|read| read.host_apps(&self.preferences.borrow()));
            kept.workspace = None;
            kept.reading.clone_from(&latest);
        }
        latest
    }

    /// Settings and the catalogue for `workspace` , built once per workspace and reading of the
    /// host's apps.
    fn for_workspace(&self, workspace: &str) -> (Option<Held>, Option<Held>) {
        let reading = self.current_reading();
        let mut kept = self.kept.borrow_mut();
        if let Some((built, settings, catalogue)) = &kept.workspace
            && built == workspace
        {
            return (Some(settings.clone()), Some(catalogue.clone()));
        }
        let settings = self
            .settings
            .as_ref()
            .map(|settings| settings.source(workspace, &self.own.borrow()));
        let catalogue = reading.as_ref().map(|read| read.catalogue(workspace));
        if let (Some(settings), Some(catalogue)) = (&settings, &catalogue) {
            kept.workspace = Some((workspace.to_owned(), settings.clone(), catalogue.clone()));
        }
        (settings, catalogue)
    }

    /// Carry out `target` , chosen in search in `workspace` : through the action host, or for the
    /// session's own actions through `own` , the session's published surface.
    pub(crate) fn perform(
        &self,
        workspace: &str,
        target: Target,
        own: &mut dyn FnMut(&InvokeTarget) -> Result<(), DoorError>,
    ) -> Result<(), DoorError> {
        self.perform_checked(workspace, target, own)
    }

    /// [`Self::perform`], saying whether it was carried out: a caller that has another way to reach
    /// the same place needs to know.
    pub(crate) fn perform_checked(
        &self,
        workspace: &str,
        target: Target,
        own: &mut dyn FnMut(&InvokeTarget) -> Result<(), DoorError>,
    ) -> Result<(), DoorError> {
        match target {
            Target::Invoke(invoke)
                if invoke.app == DESKTOP_APP
                    && invoke.address.resource.as_str() == record_rows::RECORDS_RESOURCE
                    && invoke.address.action.as_str() == record_rows::BRING_HERE =>
            {
                self.bring_here(workspace, &invoke)
            }
            Target::Invoke(invoke) if invoke.app == DESKTOP_APP => own(&invoke),
            Target::Invoke(invoke) => self.with_apps(|apps| {
                apps.invoke(
                    application_action_target(
                        &invoke.workspace,
                        &invoke.app,
                        invoke.address.resource.as_str(),
                        invoke.address.action.as_str(),
                    ),
                    invoke.arguments,
                )
            }),
            Target::Launch { app, place } => {
                self.with_apps(|apps| match host_apps::launched(&app) {
                    // The machine's own app places its window itself.
                    Some(desktop_id) => apps.start_host_app(desktop_id),
                    None => apps.open_application(workspace, &app, place),
                })
            }
            Target::Prefer { source, enabled } => {
                self.prefer(&source, enabled);
                Ok(())
            }
            Target::Cancel {
                app,
                workspace,
                invocation,
            } => self.with_apps(|apps| apps.cancel(&app, &workspace, &invocation)),
            Target::Open { uri, .. } => self.with_apps(|apps| apps.open_uri(workspace, &uri)),
            other => {
                tracing::debug!(?other, "search row the desk carries out");
                Ok(())
            }
        }
    }

    /// Bring the record a row names into `workspace` : the reference the workspace holds from then
    /// on. The record is named as the row carried it — its id and kind — so what arrives is the
    /// record, not a copy of it.
    /// A record kept as a file is also opened, so bringing it here and opening it are one gesture.
    /// The reference is named first and the launch reported after it: a record whose file is gone
    /// is still one this workspace now holds, and a refusal to open leaves it held.
    fn bring_here(&self, workspace: &str, invoke: &InvokeTarget) -> Result<(), DoorError> {
        let Some(records) = &self.records else {
            return Err(DoorError::Unavailable {
                code: "records_unavailable".into(),
                reason: "The session keeps no record indexes".into(),
            });
        };
        let text = |name: &str| {
            invoke
                .arguments
                .get(name)
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        };
        let (Some(id), Some(kind), Some(package)) = (
            text(record_rows::RECORD_PARAM),
            text(record_rows::KIND_PARAM),
            text(record_rows::PACKAGE_PARAM),
        ) else {
            return Err(DoorError::InvalidArguments(
                "a record row carries its id, its kind and its index".into(),
            ));
        };
        let Some(id) = RecordId::parse(&id) else {
            return Err(DoorError::InvalidArguments(format!(
                "{id} is not a record id"
            )));
        };
        // Rows are collected when search opens and chosen afterwards, and an app may have stopped
        // naming the record in between: a row that names a record no workspace holds any more is
        // refused rather than written, so search never brings a record that is gone into a
        // workspace as if it were still there.
        if !records.holds(id.as_str()) {
            return Err(DoorError::Unavailable {
                code: "record_not_held".into(),
                reason: format!("No workspace holds {id} any more"),
            });
        }
        let path = id.file_path();
        records.bring_here(
            workspace,
            &package,
            RecordCheckout::new(id, RecordKind::new(kind)),
        );
        // A record kept as a file is opened as well, as the document it is: `file:/…` is a record
        // id, not a link, so it is the path the app is given. It opens after the reference is
        // named, so a file that is gone leaves the record held and says so.
        let Some(path) = path else {
            return Ok(());
        };
        if !path.exists() {
            return Err(DoorError::Failed(format!("{} is gone", path.display())));
        }
        let uri = tilo_desktop_search::file_uri(&path);
        self.with_apps(|apps| apps.open_uri(workspace, &uri))
    }

    fn with_apps(
        &self,
        run: impl FnOnce(&dyn AppActions) -> Result<(), DoorError>,
    ) -> Result<(), DoorError> {
        match &self.apps {
            Some(apps) => run(&**apps),
            None => Err(DoorError::Unavailable {
                code: "action_host_unavailable".into(),
                reason: "The session runs without an action host".into(),
            }),
        }
    }
}

impl SessionSearch {
    /// The apps this session offers: the host's installed and machine apps, each by the launch its
    /// own row starts. A window group's new tab and the empty desk's launchpad search the
    /// applications alone, and the session's tile source lists none — the session is what starts
    /// them — so this is what those searches offer.
    fn applications(&self) -> Vec<tilo_desktop_search::providers::AppEntry> {
        let Some(lists) = &self.apps_lists else {
            return Vec::new();
        };
        lists
            .applications()
            .into_iter()
            .chain(lists.host_apps().into_iter().map(|app| {
                tilo_desktop_search::providers::AppEntry {
                    id: format!("{}{}", host_apps::HOST_APP_PREFIX, app.desktop_id),
                    title: app.name.clone(),
                    category: app.generic_name.clone(),
                    description: app.comment,
                }
            }))
            .collect()
    }
}

impl SearchHost for SessionSearch {
    fn applications(&self) -> Vec<tilo_desktop_search::providers::AppEntry> {
        Self::applications(self)
    }

    fn feed(&self, workspace: &str, scope: &Scope) -> SearchHostFeed {
        let (mut sources, published) = self.sources(workspace, scope);
        // A question typed by the person is answered here, by the model their AI settings choose:
        // never by a tool that searches.
        sources.push(Held::new(crate::search_ask::Ask::new(
            Arc::clone(&self.ask),
            workspace,
            self.wake.clone(),
            self.models.clone(),
        )));
        SearchHostFeed {
            // The apps installed here, which the palette offers as an app's row of its own: the
            // machine's own apps are one of the sources below instead.
            applications: self
                .apps_lists
                .as_ref()
                .map(|lists| lists.applications())
                .unwrap_or_default(),
            published,
            sources,
            preferences: self.preferences.borrow().clone(),
            now: ui::time::unix_now(),
            usage: self.usage.borrow().clone(),
        }
    }

    fn chose(&self, usage: &Usage) {
        *self.usage.borrow_mut() = Some(usage.clone());
        let (_, places) = self.unkept.get();
        self.unkept.set((true, places));
    }

    fn performs(&self, target: &Target) -> bool {
        match target {
            Target::Invoke(invoke) => invoke.app == DESKTOP_APP || self.apps.is_some(),
            Target::Launch { .. } | Target::Cancel { .. } | Target::Open { .. } => {
                self.apps.is_some()
            }
            // What the person switched off is kept with their settings,
            // which only the session holds.
            Target::Prefer { .. } => true,
            Target::Window { .. }
            | Target::Action(_)
            | Target::Command { .. }
            | Target::Copy { .. } => false,
        }
    }
}

/// The desktop's one search as a host that holds it offers it
/// ([`ResourceSearch`]): the same [`SessionSearch::find`] the palette and
/// text typed on the canvas run, so an agent's `find` answers what those do,
/// in their order.
impl ResourceSearch for SearchThrough {
    fn find(
        &self,
        caller: &Caller,
        needle: &str,
    ) -> tilo_actions::remote::Pending<Result<Vec<Found>, ActionHostError>> {
        // An agent finds what the workspace it works in holds: the search is the person's, over
        // every workspace they see, and an agent is in one.
        let workspace = match caller.principal() {
            tilo_actions::remote::CallerPrincipal::Person(_) => None,
            _ => match caller.workspace() {
                Some(workspace) => Some(workspace.to_owned()),
                None => {
                    return tilo_actions::remote::Pending::ready(Err(
                        ActionHostError::Unavailable {
                            reason:
                                "an agent searches the workspace it works in, and works in none"
                                    .to_owned(),
                        },
                    ));
                }
            },
        };
        let (done, answer) = tilo_actions::remote::pending();
        self.ask(needle.trim().to_owned(), workspace, done);
        answer.map(Ok)
    }
}

/// A host's [`ResourceSearch`] that asks the thread holding the desktop, as the session's own doors
/// are asked: the session's search is not `Send` , and every call of it runs on the loop's pass.
pub(crate) struct SearchThrough(
    std::sync::Arc<dyn Fn(String, Option<String>, FoundSearch) + Send + Sync>,
);

/// Where the session's answer to a search goes.
pub(crate) type FoundSearch = tilo_actions::remote::Completer<Vec<Found>>;

impl SearchThrough {
    pub(crate) fn new(
        ask: std::sync::Arc<dyn Fn(String, Option<String>, FoundSearch) + Send + Sync>,
    ) -> Self {
        Self(ask)
    }

    /// Ask for what `needle` finds, in `workspace` alone where one is named ([`SessionSearch::find_in`]).
    fn ask(&self, needle: String, workspace: Option<String>, done: FoundSearch) {
        (self.0)(needle, workspace, done);
    }
}

/// The search hits `found` names, as a caller who addresses resources reads them: the resource each
/// is about, best first, and never below the one `place_found` places. The canvas stands the hit
/// [`arrangement::placed`] chooses; `find` leads with that same hit, so an agent asked for a text
/// answers with what typing it would place, then with what else the search found under it. Without
/// one (a file, a command, a window's focus), the resources the search does name stand on their
/// own.
pub(crate) fn found_all(
    text: &str,
    hits: Vec<tilo_desktop_search::Hit>,
    shows: impl Fn(&str) -> bool,
) -> Vec<Found> {
    // The lead is the hit the canvas places, by the canvas's own ranking of these hits: the top one
    // that names a resource a place can show. Its own row leads even when another row about the
    // same resource ranks above it (an action on it), which is what typing the text stands. A row
    // that names a resource a place cannot show (`session`'s own places, a record no app publishes
    // as placeable) stands in the way of nothing, exactly as it does on the canvas.
    let lead = tilo_session_core::arrangement::placed(&hits, text, shows);
    let mut out: Vec<Found> = Vec::new();
    for (at, hit) in hits.into_iter().enumerate() {
        let Some(found) = found_of(&hit) else {
            continue;
        };
        // Above the lead, only the lead's own resource stands: a row the canvas would step past is
        // a row the query did not name.
        if lead.is_some_and(|lead| at < lead) {
            continue;
        }
        if !out.iter().any(|seen| seen.resource == found.resource) {
            out.push(found);
        }
    }
    out
}

/// The resource a search hit is about, as an agent addresses it (`None` for
/// a hit that names no resource an app publishes: a setting's page, a file,
/// the machine's own app).
fn found_of(hit: &tilo_desktop_search::Hit) -> Option<Found> {
    let (resource, open, arguments) = match &hit.target {
        Target::Invoke(invoke) => (
            invoke.address.resource.as_str().to_owned(),
            Some(invoke.address.action.clone()),
            invoke.arguments.clone(),
        ),
        Target::Action(action) => (
            action.resource.as_str().to_owned(),
            Some(action.spec.id.clone()),
            action
                .preset
                .as_ref()
                .map(|(name, value)| {
                    let mut arguments = serde_json::Map::new();
                    arguments.insert(name.clone(), value.clone().into());
                    arguments
                })
                .unwrap_or_default(),
        ),
        // Everything else (an app to launch, a window, a command, a worked-out answer, a file to
        // open, a job to stop) is not a resource of an app's that a path names: it is left out, as
        // the palette's own rows are for a caller that places what it finds.
        Target::Launch { .. }
        | Target::Window { .. }
        | Target::Command { .. }
        | Target::Copy { .. }
        | Target::Open { .. }
        | Target::Cancel { .. }
        | Target::Prefer { .. } => return None,
    };
    // The app a caller names the resource by: the provider that published the row, unless that is
    // the desktop publishing on an app's behalf. A workspace's record carries its app in the id it
    // published it under (`tl:<app>:…`), which a caller lists the app's directory by.
    let app = match &hit.target {
        Target::Invoke(invoke) if invoke.app != DESKTOP_APP => Some(invoke.app.clone()),
        _ => None,
    }
    .or_else(|| {
        resource
            .strip_prefix("tl:")
            .and_then(|rest| rest.split_once(':'))
            .map(|(app, _)| app.to_owned())
    });
    // The kind a caller reads is the resource's own when the row says so,
    // else the palette's word for the row ("Actions"), which names nothing
    // an agent can act on.
    let named_kind = hit.kind.label();
    let (kind, label) = if named_kind.contains('.') {
        (named_kind.to_owned(), hit.title.clone())
    } else {
        match &hit.target {
            Target::Action(action) => (action.resource_kind.clone(), action.resource_label.clone()),
            Target::Invoke(invoke) => match &invoke.descriptor {
                Some(action) => (action.resource_kind.clone(), action.resource_label.clone()),
                None => (named_kind.to_owned(), hit.title.clone()),
            },
            _ => (named_kind.to_owned(), hit.title.clone()),
        }
    };
    Some(Found {
        resource,
        kind,
        label,
        open,
        arguments,
        app,
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    use tilo_desktop_search::providers::AppEntry;
    use tilo_desktop_search::providers::host_apps::{HOST_APPS_SOURCE, HostAppEntry};

    use super::*;

    /// The action host a test session reaches: it notes what search opens in which workspace, and
    /// carries out nothing else.
    struct Opening(Rc<std::cell::RefCell<Vec<(String, String)>>>);

    fn unsupported() -> DoorError {
        DoorError::Unavailable {
            code: "action_host_unavailable".into(),
            reason: "The test host runs no actions".into(),
        }
    }

    impl AppActions for Opening {
        fn published(&self, _: &str) -> Vec<PublishedActions> {
            Vec::new()
        }

        fn jobs(&self) -> Vec<JobEntry> {
            Vec::new()
        }

        fn invoke(
            &self,
            _: RemoteActionTarget,
            _: serde_json::Map<String, serde_json::Value>,
        ) -> Result<(), DoorError> {
            Err(unsupported())
        }

        fn cancel(&self, _: &str, _: &str, _: &str) -> Result<(), DoorError> {
            Err(unsupported())
        }

        fn open_uri(&self, workspace: &str, uri: &str) -> Result<(), DoorError> {
            self.0
                .borrow_mut()
                .push((workspace.to_owned(), uri.to_owned()));
            Ok(())
        }

        fn start_host_app(&self, _: &str) -> Result<(), DoorError> {
            Err(unsupported())
        }
    }

    /// The lists a test device has: an installed app and a host app for each line of `file` .
    fn lists_in(file: PathBuf) -> impl Fn() -> crate::lists::AppLists + Send + Sync + 'static {
        move || {
            let text = std::fs::read_to_string(&file).unwrap_or_default();
            crate::lists::AppLists {
                applications: text
                    .lines()
                    .map(|name| AppEntry {
                        id: format!("so.tilo.{}", name.to_lowercase()),
                        title: name.to_owned(),
                        category: None,
                        description: None,
                    })
                    .collect(),
                host_apps: text
                    .lines()
                    .map(|name| HostAppEntry {
                        desktop_id: format!("org.example.{name}.desktop"),
                        name: format!("{name} Viewer"),
                        generic_name: None,
                        comment: None,
                        keywords: Vec::new(),
                    })
                    .collect(),
                catalogue: Vec::new(),
                icons: std::collections::BTreeMap::new(),
                candidates: Vec::new(),
                declared: std::collections::BTreeMap::new(),
                launchable: Vec::new(),
                tools: Vec::new(),
            }
        }
    }

    /// The installed apps and the host apps matching `needle` that a search opened now is fed.
    fn found(search: &SessionSearch, needle: &str) -> (Vec<String>, Vec<String>) {
        let feed = search.feed("workspace", &Scope::Workspace);
        let applications = feed.applications.into_iter().map(|app| app.title).collect();
        let host = feed
            .sources
            .iter()
            .filter(|source| source.id() == HOST_APPS_SOURCE)
            .flat_map(|source| source.query(needle))
            .map(|hit| hit.title)
            .collect();
        (applications, host)
    }

    #[test]
    fn a_change_to_the_lists_reaches_the_next_search() {
        let dir = tempfile::tempdir().expect("a directory");
        let file = dir.path().join("apps");
        std::fs::write(&file, "Sheets\n").expect("the first list");
        let lists = crate::lists::Lists::follow(
            "search-lists-test",
            vec![dir.path().to_path_buf()],
            lists_in(file.clone()),
        )
        .expect("lists followed");
        let search = SessionSearch::new(
            SearchOptions::default(),
            Some(Arc::new(lists)),
            "",
            None,
            None,
            None,
        );
        // Starting the lists reads them on a worker, so the first search waits for that read to
        // land before it can see what they hold.
        tilo_test_env::wait_until("the lists' first read", || {
            found(&search, "viewer").0 == ["Sheets"]
        });
        assert_eq!(
            found(&search, "viewer"),
            (vec!["Sheets".to_owned()], vec!["Sheets Viewer".to_owned()])
        );

        std::fs::write(dir.path().join("apps.new"), "Calendar\n").expect("the next list");
        std::fs::rename(dir.path().join("apps.new"), &file).expect("the list replaced");
        let started = Instant::now();
        let mut now = found(&search, "viewer");
        while now.0 != ["Calendar"] && started.elapsed() < Duration::from_secs(10) {
            std::thread::park_timeout(Duration::from_millis(10));
            now = found(&search, "viewer");
        }
        assert_eq!(
            now,
            (
                vec!["Calendar".to_owned()],
                vec!["Calendar Viewer".to_owned()]
            )
        );
    }

    /// A row is collected from one reading of the device's indexes and chosen later, so the record
    /// it names may have been dropped from every index in between: it is refused rather than named
    /// in a workspace as if it were still there.
    #[test]
    fn a_row_for_a_record_no_workspace_names_any_more_is_refused() {
        let home = tilo_test_env::TestHome::new().expect("a home");
        // An index this device keeps, naming a record that is not the row's: the row names one no
        // workspace holds.
        let directory =
            app_state::workspace_index_directory(home.data(), "so.tilo.notes", "elsewhere")
                .expect("an index directory");
        std::fs::create_dir_all(&directory).expect("the index's directory");
        let mut index = automerge::AutoCommit::new();
        let kept = ui::record::RecordId::parse("file:/work/Kept.odt").expect("a record id");
        assert!(ui::record::index::reference(
            &mut index,
            &kept,
            &ui::record::RecordKind::new("document")
        ));
        std::fs::write(directory.join(app_state::DOCUMENT_FILE), index.save()).expect("the index");

        let search = SessionSearch::new(
            SearchOptions {
                records: Some(crate::records::Records::of(home.data().to_path_buf())),
                ..SearchOptions::default()
            },
            None,
            "",
            None,
            None,
            None,
        );
        let row = Target::Invoke(Box::new(
            InvokeTarget::action(
                DESKTOP_APP,
                "elsewhere",
                record_rows::RECORDS_RESOURCE,
                record_rows::BRING_HERE,
            )
            .with_argument(record_rows::RECORD_PARAM, "file:/work/Gone.odt")
            .with_argument(record_rows::KIND_PARAM, "document")
            .with_argument(record_rows::PACKAGE_PARAM, "so.tilo.notes"),
        ));
        let refused = search.perform_checked("here", row, &mut |_| Ok(()));
        assert!(
            matches!(
                refused,
                Err(DoorError::Unavailable { ref code, .. }) if code == "record_not_held"
            ),
            "a record no workspace names is refused: {refused:?}"
        );
    }

    /// A record kept as a file is brought here and opened: the row a person chose in search leaves
    /// the workspace naming the record *and* shows them the document, in the app that takes its
    /// type.
    #[test]
    fn a_file_record_chosen_in_search_is_brought_here_and_opened() {
        let home = tilo_test_env::TestHome::new().expect("a home");
        let file = home.home().join("Budget.note");
        std::fs::write(&file, "what the record holds").expect("the record's file");
        let record = ui::record::RecordId::for_file(&file);
        // The workspace that holds it names it in its index.
        let directory =
            app_state::workspace_index_directory(home.data(), "so.tilo.notes", "elsewhere")
                .expect("an index directory");
        std::fs::create_dir_all(&directory).expect("the index's directory");
        let mut index = automerge::AutoCommit::new();
        assert!(ui::record::index::reference(
            &mut index,
            &record,
            &ui::record::RecordKind::new("document")
        ));
        std::fs::write(directory.join(app_state::DOCUMENT_FILE), index.save()).expect("the index");

        let opened = Rc::new(std::cell::RefCell::new(Vec::new()));
        let search = SessionSearch::new(
            SearchOptions {
                records: Some(crate::records::Records::of(home.data().to_path_buf())),
                ..SearchOptions::default()
            },
            None,
            "",
            Some(Rc::new(Opening(Rc::clone(&opened)))),
            None,
            None,
        );
        let row = Target::Invoke(Box::new(
            InvokeTarget::action(
                DESKTOP_APP,
                "here",
                record_rows::RECORDS_RESOURCE,
                record_rows::BRING_HERE,
            )
            .with_argument(record_rows::RECORD_PARAM, record.as_str())
            .with_argument(record_rows::KIND_PARAM, "document")
            .with_argument(record_rows::PACKAGE_PARAM, "so.tilo.notes"),
        ));
        search
            .perform_checked("here", row, &mut |_| Ok(()))
            .expect("the record is brought here and opened");
        assert_eq!(
            opened.borrow().as_slice(),
            [("here".to_owned(), tilo_desktop_search::file_uri(&file))],
            "the document is opened by its path, not by the record id's `file:/…`"
        );

        // The file going away leaves the record held and says what is gone.
        std::fs::remove_file(&file).expect("the file goes");
        let row = Target::Invoke(Box::new(
            InvokeTarget::action(
                DESKTOP_APP,
                "here",
                record_rows::RECORDS_RESOURCE,
                record_rows::BRING_HERE,
            )
            .with_argument(record_rows::RECORD_PARAM, record.as_str())
            .with_argument(record_rows::KIND_PARAM, "document")
            .with_argument(record_rows::PACKAGE_PARAM, "so.tilo.notes"),
        ));
        assert!(
            matches!(
                search.perform_checked("here", row, &mut |_| Ok(())),
                Err(DoorError::Failed(ref reason)) if reason.contains("is gone")
            ),
            "a record whose file is gone is still held, and the launch says why it does not open"
        );
    }

    /// The places the search over `scope` lists, each with the facts it shows about it.
    fn places_listed(
        search: &SessionSearch,
        workspace: &str,
        scope: &Scope,
    ) -> Vec<(String, Vec<String>)> {
        let feed = search.feed(workspace, scope);
        feed.sources
            .iter()
            .find(|source| source.id() == "continue")
            .map(|source| {
                source
                    .query("")
                    .into_iter()
                    .map(|hit| (hit.title, hit.facts))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn place(label: &str, workspace: &str) -> Place {
        Place {
            app: "so.tilo.notes".into(),
            workspace: workspace.into(),
            tab: 4,
            label: label.into(),
            value: String::new(),
            anchor: Some(format!("n:{label}")),
            at: 1,
            device: None,
        }
    }

    /// The desk's own rows (a window, by the title of what it shows) span every workspace: the
    /// person's search finds them, and an agent's, confined to one workspace, never does.
    #[test]
    fn an_agents_search_finds_nothing_of_the_desks_own_rows() {
        let search = SessionSearch::new(SearchOptions::default(), None, "", None, None, None);
        let window =
            tilo_actions::ResourceSpec::new("window:7", "desktop.window", "Quarterly plan — Notes")
                .with_action(tilo_actions::ActionSpec::immediate_encoded(
                    "focus",
                    "Quarterly plan — Notes",
                    tilo_actions::ExecutionLocus::ClientLocal,
                    tilo_actions::EncodedAction {
                        type_id: "desktop.window".into(),
                        payload: Vec::new(),
                    },
                ));
        search.observe(&[window], Vec::new(), None, Vec::new());
        let titles = |hits: Vec<tilo_desktop_search::Hit>| -> Vec<String> {
            hits.into_iter().map(|hit| hit.title).collect()
        };
        let person = titles(search.find("Quarterly plan"));
        assert!(
            person.iter().any(|title| title.contains("Quarterly plan")),
            "the person's search finds the window: {person:?}"
        );
        let agent = titles(search.find_in("Quarterly plan", Some("garden")));
        assert!(
            !agent.iter().any(|title| title.contains("Quarterly plan")),
            "an agent in one workspace finds no window of the desk: {agent:?}"
        );
    }

    #[test]
    fn widening_search_lists_other_workspaces_places_by_name() {
        let mut recent = RecentPlaces::default();
        recent.record(place("Groceries", "here"));
        recent.record(place("Quarterly plan", "elsewhere"));
        let search = SessionSearch::new(
            SearchOptions {
                places: recent,
                ..SearchOptions::default()
            },
            None,
            "",
            None,
            None,
            None,
        );
        search.set_workspaces(vec![
            ("here".into(), "Main".into()),
            ("elsewhere".into(), "Lounge".into()),
        ]);

        // Narrowed to the workspace in front, its own places only.
        assert_eq!(
            places_listed(&search, "here", &Scope::Workspace)
                .into_iter()
                .map(|(title, _)| title)
                .collect::<Vec<_>>(),
            ["Groceries"]
        );

        // Widened, another workspace's place is found beside its own, saying where it is.
        let wide = places_listed(&search, "here", &Scope::Everywhere);
        let elsewhere = wide
            .iter()
            .find(|(title, _)| title == "Quarterly plan")
            .expect("another workspace's place is found");
        assert!(
            elsewhere.1.iter().any(|fact| fact.contains("Lounge")),
            "the row says which workspace holds it: {:?}",
            elsewhere.1
        );
        assert!(
            wide.iter().any(|(title, _)| title == "Groceries"),
            "the workspace in front keeps its places: {wide:?}"
        );
    }

    #[test]
    fn fixed_lists_keep_what_search_built_from_them() {
        let lists = crate::lists::Lists::fixed(crate::lists::AppLists::default());
        let search = SessionSearch::new(
            SearchOptions::default(),
            Some(Arc::new(lists)),
            "",
            None,
            None,
            None,
        );
        let before = search
            .kept
            .borrow()
            .reading
            .clone()
            .expect("the reading the sources were built from")
            .generation();
        let _ = search.feed("workspace", &Scope::Workspace);
        let kept = search.kept.borrow();
        assert_eq!(
            before,
            kept.reading
                .clone()
                .expect("still the same reading")
                .generation(),
            "a list that did not change is not read again"
        );
    }
}
