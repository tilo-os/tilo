//! The desktop's search in the native session: the shared desktop search
//! ([`tilo_desktop_search`]) the desk opens, over what is published and
//! what the session's services answer, each row run by its address.
//!
//! The desk lists the windows, the people and the desktop's own commands.
//! The session adds what only a device has: the apps installed here and the
//! machine's own (with the person's switch), Settings' pages, the store's
//! catalogue, what apps hold (the content index), the person's files, where
//! their windows were, the actions open apps publish through the action
//! host and the jobs it runs, and the session's own published actions. A
//! chosen row the session carries out: its own action through its own
//! published surface, an app's through the action host by address (the app
//! started first when it is closed), an app's start, an address.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use crate::lists::{AppLists, Lists};
use crate::records::Records;

use tilo_actions::DoorError;
use tilo_actions::remote::{
    RemoteActionTarget, application_action_target, application_open_target,
};
use tilo_desktop_search::content_index::AppContentIndex;
use tilo_desktop_search::files_index::FilesIndex;
use tilo_desktop_search::indexed::IndexedContent;
use tilo_desktop_search::providers::PublishedActions;
use tilo_desktop_search::providers::catalogue::StoreCatalogue;
use tilo_desktop_search::providers::files::{FileNames, Files, file_uri};
use tilo_desktop_search::providers::host_apps::{self, HostApps};
use tilo_desktop_search::providers::jobs::{JobEntry, Jobs};
use tilo_desktop_search::providers::places::{Continue, Place, RecentPlaces};
use tilo_desktop_search::providers::records as record_rows;
use tilo_desktop_search::providers::settings::Settings;
use tilo_desktop_search::providers::waiting::Waiting;
use tilo_desktop_search::{DESKTOP_APP, InvokeTarget, SearchPreferences, Source, Target, Usage};
use tilo_desktop_surface::{SearchHost, SearchHostFeed};
use ui::record::{RecordCheckout, RecordId, RecordKind};

/// The action host as the session's search reaches it: what open apps
/// publish and run, and running what is chosen by address.
pub trait AppActions {
    /// What open apps publish through the action host, by app and room.
    fn published(&self) -> Vec<PublishedActions>;
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
    /// Ask the invocation `invocation` of `app` in `room` to stop.
    ///
    /// # Errors
    /// The host's refusal: no such invocation, or it cannot be stopped.
    fn cancel(&self, app: &str, room: &str, invocation: &str) -> Result<(), DoorError>;
    /// Open `uri` in room `room`, in the app that takes its scheme: a file,
    /// a Store page, a web address.
    ///
    /// # Errors
    /// Nothing takes it.
    fn open_uri(&self, room: &str, uri: &str) -> Result<(), DoorError>;
    /// Start the machine's own app `desktop_id`.
    ///
    /// # Errors
    /// It did not start.
    fn start_host_app(&self, desktop_id: &str) -> Result<(), DoorError>;
}

/// What the session's search reaches beyond the desktop.
#[derive(Default)]
pub struct SearchOptions {
    /// The Tilo apps installed here, the machine's own and the apps a
    /// person can get, as they are now.
    pub lists: Lists,
    /// What apps hold, as they published it.
    pub content: Option<Arc<AppContentIndex>>,
    /// The person's home folder, its names as walked, and the index of
    /// what the files hold.
    pub files: Option<(PathBuf, FileNames, Option<Arc<FilesIndex>>)>,
    /// The sources the person switched off.
    pub preferences: SearchPreferences,
    /// Where the person's windows were.
    pub places: RecentPlaces,
    /// The records the workspaces on this device hold: what search finds by
    /// name across them, and where a record brought here is named.
    pub records: Option<Records>,
}

/// The sources the session keeps between searches, each indexed once per
/// reading of the lists they are built from.
struct Kept {
    /// The lists the sources below were built from.
    lists: Arc<AppLists>,
    host_apps: Rc<HostApps>,
    content: Option<Rc<IndexedContent>>,
    files: Option<Rc<Files>>,
    /// Settings' pages and the store's catalogue, for the room they were
    /// built for.
    room: Option<(String, Rc<Settings>, Rc<StoreCatalogue>)>,
}

/// The session's part in the desk's search.
pub(crate) struct SessionSearch {
    lists: Lists,
    preferences: SearchPreferences,
    places: RefCell<RecentPlaces>,
    /// What the person chose before; `None` until they chose or it was
    /// restored.
    usage: RefCell<Option<Usage>>,
    /// The parts of the history changed since they were last kept: the
    /// choices, the places.
    unkept: Cell<(bool, bool)>,
    kept: RefCell<Kept>,
    apps: Option<Rc<dyn AppActions>>,
    /// The records the workspaces on this device hold.
    records: Option<Records>,
    /// What the session itself publishes, as of the last frame: the desk
    /// cannot read its own tree while it opens search.
    own: RefCell<Vec<tilo_actions::ActionDescriptor>>,
    /// The windows open, as of the last frame.
    open_tabs: RefCell<Vec<u64>>,
    /// The live notifications the session publishes, as of the last frame.
    notifications: RefCell<Vec<tilo_actions::ResourceSpec>>,
}

/// The resource of the section keeping search's history.
pub(crate) const HISTORY: &str = "settings:settings.search_history";

/// The field edit setting the history's field `field` to `value`.
fn set_field(field: &str, value: &serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
    let mut edit = serde_json::Map::new();
    edit.insert(
        "edit".into(),
        serde_json::json!({ "path": [{ "field": field }], "op": "set", "value": value }),
    );
    edit
}

/// An app's name as people know it.
fn app_name(app: &str) -> String {
    tilo_desktop_search::availability::name_for(app).map_or_else(|| app.to_owned(), str::to_owned)
}

impl SessionSearch {
    pub(crate) fn new(options: SearchOptions, apps: Option<Rc<dyn AppActions>>) -> Self {
        let SearchOptions {
            lists,
            content,
            files,
            preferences,
            places,
            records,
        } = options;
        let read = lists.latest();
        let kept = Kept {
            host_apps: Rc::new(HostApps::new(read.host_apps.clone(), &preferences)),
            lists: read,
            content: content.map(|index| Rc::new(IndexedContent::new(index, app_name))),
            files: files.map(|(home, names, content)| Rc::new(Files::new(home, names, content))),
            room: None,
        };
        Self {
            lists,
            preferences,
            places: RefCell::new(places),
            usage: RefCell::new(None),
            unkept: Cell::new((false, false)),
            kept: RefCell::new(kept),
            apps,
            records,
            own: RefCell::default(),
            open_tabs: RefCell::default(),
            notifications: RefCell::default(),
        }
    }

    /// What the session publishes now, and the windows open: read after
    /// each frame, outside the desktop's trees.
    pub(crate) fn observe(&self, own: &[tilo_actions::ResourceSpec], open_tabs: Vec<u64>) {
        *self.notifications.borrow_mut() = own
            .iter()
            .find(|resource| {
                resource.id.as_str() == tilo_notifications_service::NOTIFICATIONS_RESOURCE
            })
            .map(|notifications| notifications.children.clone())
            .unwrap_or_default();
        let mut published = tilo_actions::door::survey(own);
        // Search does not list itself, nor opening itself.
        published.retain(|action| {
            let address = &action.address;
            let searching = address
                .resource
                .starts_with(tilo_desktop::desktop_actions::SEARCH_RESOURCE);
            let opening = address.resource == tilo_desktop::desktop_actions::DESK_RESOURCE
                && address.action == "open-search";
            !searching && !opening
        });
        *self.own.borrow_mut() = published;
        *self.open_tabs.borrow_mut() = open_tabs;
    }

    /// The workspaces this device shows, by room and the name the desk gives
    /// it: what a row says of a record the workspace in front does not hold.
    pub(crate) fn set_workspaces(&self, workspaces: Vec<(String, String)>) {
        if let Some(records) = &self.records {
            records.set_workspaces(workspaces);
        }
    }

    /// A window was at `place`.
    pub(crate) fn record_place(&self, place: Place) {
        let mut places = self.places.borrow_mut();
        let before = places.places().first().cloned();
        places.record(place);
        if places.places().first() != before.as_ref() {
            let (chosen, _) = self.unkept.get();
            self.unkept.set((chosen, true));
        }
    }

    /// Take up the history as the search history section ([`HISTORY`])
    /// keeps it.
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
            .and_then(|places| serde_json::from_value(places.clone()).ok())
        {
            *self.places.borrow_mut() = places;
        }
    }

    /// The field edits that keep what changed of the history since it was
    /// last kept, for the search history section's `field-edit`.
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

    /// The lists as last read, with the sources built from them rebuilt
    /// when they were read again since: the machine's apps here, Settings
    /// and the catalogue at their room's next search.
    fn current_lists(&self) -> Arc<AppLists> {
        let latest = self.lists.latest();
        let mut kept = self.kept.borrow_mut();
        if !Arc::ptr_eq(&latest, &kept.lists) {
            kept.host_apps = Rc::new(HostApps::new(latest.host_apps.clone(), &self.preferences));
            kept.room = None;
            kept.lists = Arc::clone(&latest);
        }
        latest
    }

    /// Settings and the catalogue for `room`, built once per room and
    /// reading of the lists.
    fn for_room(&self, room: &str) -> (Rc<Settings>, Rc<StoreCatalogue>) {
        let lists = self.current_lists();
        let mut kept = self.kept.borrow_mut();
        if let Some((built, settings, catalogue)) = &kept.room
            && built == room
        {
            return (Rc::clone(settings), Rc::clone(catalogue));
        }
        // Settings the settings service changes in place are found as its
        // actions.
        let in_place: std::collections::HashSet<String> = self
            .own
            .borrow()
            .iter()
            .filter_map(|action| {
                tilo_settings_service::setting_of(action.address.resource.as_str())
                    .map(str::to_owned)
            })
            .collect();
        let settings = Rc::new(Settings::changing_in_place(room, &|id| {
            in_place.contains(id)
        }));
        let catalogue = Rc::new(StoreCatalogue::new(lists.catalogue.clone(), room));
        kept.room = Some((room.to_owned(), Rc::clone(&settings), Rc::clone(&catalogue)));
        (settings, catalogue)
    }

    /// Carry out `target`, chosen in search in the workspace of `room`:
    /// through the action host, or for the session's own actions through
    /// `own`, the session's published surface.
    pub(crate) fn perform(
        &self,
        room: &str,
        target: Target,
        own: &mut dyn FnMut(&InvokeTarget) -> Result<(), DoorError>,
    ) {
        if let Err(error) = self.perform_checked(room, target, own) {
            tracing::warn!(%error, "search row not carried out");
        }
    }

    /// [`Self::perform`], saying whether it was carried out: a caller that
    /// has another way to reach the same place needs to know.
    pub(crate) fn perform_checked(
        &self,
        room: &str,
        target: Target,
        own: &mut dyn FnMut(&InvokeTarget) -> Result<(), DoorError>,
    ) -> Result<(), DoorError> {
        match target {
            Target::Invoke(invoke)
                if invoke.app == DESKTOP_APP
                    && invoke.address.resource.as_str() == record_rows::RECORDS_RESOURCE
                    && invoke.address.action.as_str() == record_rows::BRING_HERE =>
            {
                self.bring_here(room, &invoke)
            }
            Target::Invoke(invoke) if invoke.app == DESKTOP_APP => own(&invoke),
            Target::Invoke(invoke) => self.with_apps(|apps| {
                apps.invoke(
                    application_action_target(
                        &invoke.room,
                        &invoke.app,
                        invoke.address.resource.as_str(),
                        invoke.address.action.as_str(),
                    ),
                    invoke.arguments,
                )
            }),
            Target::Launch { app } => self.with_apps(|apps| match host_apps::launched(&app) {
                Some(desktop_id) => apps.start_host_app(desktop_id),
                None => apps.invoke(application_open_target(room, &app), serde_json::Map::new()),
            }),
            Target::Cancel {
                app,
                room,
                invocation,
            } => self.with_apps(|apps| apps.cancel(&app, &room, &invocation)),
            Target::Open { uri, .. } => self.with_apps(|apps| apps.open_uri(room, &uri)),
            other => {
                tracing::debug!(?other, "search row the desk carries out");
                Ok(())
            }
        }
    }

    /// Bring the record a row names into the workspace of `room`: the
    /// reference the workspace holds from then on. The record is named as the
    /// row carried it — its id and kind — so what arrives is the record, not
    /// a copy of it.
    ///
    /// A record kept as a file is also opened, so bringing it here and
    /// opening it are one gesture. The reference is named first and the
    /// launch reported after it: a record whose file is gone is still one
    /// this workspace now holds, and a refusal to open leaves it held.
    fn bring_here(&self, room: &str, invoke: &InvokeTarget) -> Result<(), DoorError> {
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
        // Rows are collected when search opens and chosen afterwards, and an
        // app may have stopped naming the record in between: a row that names
        // a record no workspace holds any more is refused rather than
        // written, so search never brings a record that is gone into a
        // workspace as if it were still there.
        if !records.holds(id.as_str()) {
            return Err(DoorError::Unavailable {
                code: "record_not_held".into(),
                reason: format!("No workspace holds {id} any more"),
            });
        }
        let path = id.file_path();
        records.bring_here(
            room,
            &package,
            RecordCheckout::new(id, RecordKind::new(kind)),
        );
        // A record kept as a file is opened as well, as the document it is:
        // `file:/…` is a record id, not a link, so it is the path the app is
        // given. It opens after the reference is named, so a file that is
        // gone leaves the record held and says so.
        let Some(path) = path else {
            return Ok(());
        };
        if !path.exists() {
            return Err(DoorError::Failed(format!("{} is gone", path.display())));
        }
        let uri = file_uri(&path);
        self.with_apps(|apps| apps.open_uri(room, &uri))
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

impl SearchHost for SessionSearch {
    fn feed(&self, room: &str) -> SearchHostFeed {
        let (settings, catalogue) = self.for_room(room);
        let kept = self.kept.borrow();
        let mut sources: Vec<Box<dyn Source>> = vec![
            Box::new(settings),
            Box::new(catalogue),
            Box::new(Rc::clone(&kept.host_apps)),
            Box::new(Waiting::new(room, self.notifications.borrow().clone())),
            Box::new(Continue::new(
                &self.places.borrow(),
                room,
                self.open_tabs.borrow().clone(),
                app_name,
            )),
        ];
        if let Some(content) = &kept.content {
            sources.push(Box::new(Rc::clone(content)));
        }
        if let Some(files) = &kept.files {
            sources.push(Box::new(Rc::clone(files)));
        }
        if let Some(records) = &self.records {
            sources.push(Box::new(record_rows::Records::new(
                room,
                records.held(room),
            )));
        }
        let mut published = vec![PublishedActions {
            app: DESKTOP_APP.into(),
            room: room.to_owned(),
            actions: self.own.borrow().clone(),
        }];
        if let Some(apps) = &self.apps {
            sources.push(Box::new(Jobs(apps.jobs())));
            published.extend(apps.published());
        }
        SearchHostFeed {
            applications: kept.lists.applications.clone(),
            published,
            sources,
            preferences: self.preferences.clone(),
            now: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_secs()),
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
            Target::Window { .. }
            | Target::Action(_)
            | Target::Command { .. }
            | Target::Copy { .. } => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use tilo_desktop_search::providers::AppEntry;
    use tilo_desktop_search::providers::host_apps::{HOST_APPS_SOURCE, HostAppEntry};

    use super::*;

    /// The action host a test session reaches: it notes what search opens in
    /// which room, and carries out nothing else.
    struct Opening(Rc<std::cell::RefCell<Vec<(String, String)>>>);

    fn unsupported() -> DoorError {
        DoorError::Unavailable {
            code: "action_host_unavailable".into(),
            reason: "The test host runs no actions".into(),
        }
    }

    impl AppActions for Opening {
        fn published(&self) -> Vec<PublishedActions> {
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

        fn open_uri(&self, room: &str, uri: &str) -> Result<(), DoorError> {
            self.0.borrow_mut().push((room.to_owned(), uri.to_owned()));
            Ok(())
        }

        fn start_host_app(&self, _: &str) -> Result<(), DoorError> {
            Err(unsupported())
        }
    }

    /// The lists a test device has: an installed app and a host app for
    /// each line of `file`.
    fn lists_in(file: PathBuf) -> impl Fn() -> AppLists + Send + Sync + 'static {
        move || {
            let text = std::fs::read_to_string(&file).unwrap_or_default();
            AppLists {
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
            }
        }
    }

    /// The installed apps and the host apps matching `needle` that a search
    /// opened now is fed.
    fn found(search: &SessionSearch, needle: &str) -> (Vec<String>, Vec<String>) {
        let feed = search.feed("room");
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
        let lists = Lists::follow(
            "search-lists-test",
            vec![dir.path().to_path_buf()],
            lists_in(file.clone()),
        )
        .expect("lists followed");
        let search = SessionSearch::new(
            SearchOptions {
                lists,
                ..SearchOptions::default()
            },
            None,
        );
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

    /// A row is collected from one reading of the device's indexes and chosen
    /// later, so the record it names may have been dropped from every index
    /// in between: it is refused rather than named in a workspace as if it
    /// were still there.
    #[test]
    fn a_row_for_a_record_no_workspace_names_any_more_is_refused() {
        let home = tilo_test_env::TestHome::new().expect("a home");
        // An index this device keeps, naming a record that is not the row's:
        // the row names one no workspace holds.
        let directory = app_state::room_index_directory(home.data(), "so.tilo.notes", "elsewhere")
            .expect("an index directory");
        std::fs::create_dir_all(&directory).expect("the index's directory");
        let mut index = automerge::AutoCommit::new();
        let kept = ui::record::RecordId::parse("file:/work/Kept.odt").expect("a record id");
        assert!(ui::record::room::reference(
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

    /// A record kept as a file is brought here and opened: the row a person
    /// chose in search leaves the workspace naming the record *and* shows
    /// them the document, in the app that takes its type.
    #[test]
    fn a_file_record_chosen_in_search_is_brought_here_and_opened() {
        let home = tilo_test_env::TestHome::new().expect("a home");
        let file = home.home().join("Budget.note");
        std::fs::write(&file, "what the record holds").expect("the record's file");
        let record = ui::record::RecordId::for_file(&file);
        // The workspace that holds it names it in its index.
        let directory = app_state::room_index_directory(home.data(), "so.tilo.notes", "elsewhere")
            .expect("an index directory");
        std::fs::create_dir_all(&directory).expect("the index's directory");
        let mut index = automerge::AutoCommit::new();
        assert!(ui::record::room::reference(
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
            Some(Rc::new(Opening(Rc::clone(&opened)))),
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
            [("here".to_owned(), file_uri(&file))],
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

    #[test]
    fn fixed_lists_keep_what_search_built_from_them() {
        let search = SessionSearch::new(SearchOptions::default(), None);
        let before = Rc::clone(&search.kept.borrow().host_apps);
        let _ = search.feed("room");
        assert!(Rc::ptr_eq(&before, &search.kept.borrow().host_apps));
    }
}
