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

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use tilo_actions::DoorError;
use tilo_actions::remote::{
    RemoteActionTarget, application_action_target, application_open_target,
};
use tilo_desktop_search::content_index::AppContentIndex;
use tilo_desktop_search::files_index::FilesIndex;
use tilo_desktop_search::indexed::IndexedContent;
use tilo_desktop_search::providers::catalogue::StoreCatalogue;
use tilo_desktop_search::providers::files::{FileNames, Files};
use tilo_desktop_search::providers::host_apps::{self, HostAppEntry, HostApps};
use tilo_desktop_search::providers::jobs::{JobEntry, Jobs};
use tilo_desktop_search::providers::places::{Continue, Place, RecentPlaces};
use tilo_desktop_search::providers::settings::Settings;
use tilo_desktop_search::providers::{AppEntry, PublishedActions};
use tilo_desktop_search::{DESKTOP_APP, InvokeTarget, SearchPreferences, Source, Target};
use tilo_desktop_surface::{SearchHost, SearchHostFeed};

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
    /// The Tilo apps installed here.
    pub applications: Vec<AppEntry>,
    /// The machine's own apps.
    pub host_apps: Vec<HostAppEntry>,
    /// The apps a person can get here: neither installed nor blocked.
    pub catalogue: Vec<app_catalogue::CatalogueEntry>,
    /// What apps hold, as they published it.
    pub content: Option<Arc<AppContentIndex>>,
    /// The person's home folder, its names as walked, and the index of
    /// what the files hold.
    pub files: Option<(PathBuf, FileNames, Option<Arc<FilesIndex>>)>,
    /// The sources the person switched off.
    pub preferences: SearchPreferences,
    /// Where the person's windows were.
    pub places: RecentPlaces,
}

/// The sources the session keeps between searches, each indexed once.
struct Kept {
    host_apps: Rc<HostApps>,
    content: Option<Rc<IndexedContent>>,
    files: Option<Rc<Files>>,
    /// Settings' pages and the store's catalogue, for the room they were
    /// built for.
    room: Option<(String, Rc<Settings>, Rc<StoreCatalogue>)>,
}

/// The session's part in the desk's search.
pub(crate) struct SessionSearch {
    applications: Vec<AppEntry>,
    catalogue: Vec<app_catalogue::CatalogueEntry>,
    preferences: SearchPreferences,
    places: RefCell<RecentPlaces>,
    kept: RefCell<Kept>,
    apps: Option<Rc<dyn AppActions>>,
    /// What the session itself publishes, as of the last frame: the desk
    /// cannot read its own tree while it opens search.
    own: RefCell<Vec<tilo_actions::ActionDescriptor>>,
    /// The windows open, as of the last frame.
    open_tabs: RefCell<Vec<u64>>,
}

/// An app's name as people know it.
fn app_name(app: &str) -> String {
    tilo_desktop_search::availability::name_for(app).map_or_else(|| app.to_owned(), str::to_owned)
}

impl SessionSearch {
    pub(crate) fn new(options: SearchOptions, apps: Option<Rc<dyn AppActions>>) -> Self {
        let SearchOptions {
            applications,
            host_apps,
            catalogue,
            content,
            files,
            preferences,
            places,
        } = options;
        let kept = Kept {
            host_apps: Rc::new(HostApps::new(host_apps, &preferences)),
            content: content.map(|index| Rc::new(IndexedContent::new(index, app_name))),
            files: files.map(|(home, names, content)| Rc::new(Files::new(home, names, content))),
            room: None,
        };
        Self {
            applications,
            catalogue,
            preferences,
            places: RefCell::new(places),
            kept: RefCell::new(kept),
            apps,
            own: RefCell::default(),
            open_tabs: RefCell::default(),
        }
    }

    /// What the session publishes now, and the windows open: read after
    /// each frame, outside the desktop's trees.
    pub(crate) fn observe(&self, own: &[tilo_actions::ResourceSpec], open_tabs: Vec<u64>) {
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

    /// A window was at `place`.
    pub(crate) fn record_place(&self, place: Place) {
        self.places.borrow_mut().record(place);
    }

    /// Settings and the catalogue for `room`, built once per room.
    fn for_room(&self, room: &str) -> (Rc<Settings>, Rc<StoreCatalogue>) {
        let mut kept = self.kept.borrow_mut();
        if let Some((built, settings, catalogue)) = &kept.room
            && built == room
        {
            return (Rc::clone(settings), Rc::clone(catalogue));
        }
        let settings = Rc::new(Settings::new(room));
        let catalogue = Rc::new(StoreCatalogue::new(self.catalogue.clone(), room));
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
        let done = match target {
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
        };
        if let Err(error) = done {
            tracing::warn!(%error, "search row not carried out");
        }
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
            applications: self.applications.clone(),
            published,
            sources,
            preferences: self.preferences.clone(),
            now: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_secs()),
        }
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
