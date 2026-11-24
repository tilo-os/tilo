//! The shell's search in a session: the shared desktop search ([`tilo_desktop_search`]) the shell
//! opens, over what is published and what the session's services answer, each row run by its
//! address. A native desktop and the page both host this one role.
//! The shell lists the windows, the people and its own commands. The search adds what a
//! session offers beyond the shell: Settings' pages, the apps this host lists and the machine's own
//! (with the person's switch), the store's catalogue, what apps hold (the content index), the
//! person's files, where their windows were, the actions open apps publish through the action host
//! and the jobs it runs, and the session's own published actions. A chosen row the session carries
//! out: its own action through its own published surface, an app's through the action host by
//! address (the app started first when it is closed), an app's start, an address.

#![cfg_attr(
    target_arch = "wasm32",
    allow(
        dead_code,
        reason = "the page hosts this search with no action host or model gateway, so what those reach stays unread there"
    )
)]

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use crate::platform::apps::{Apps, Reading};
use crate::platform::indexes::Indexes;
use crate::records::Records;

/// What the session publishes that search reads, fed every pass.
#[cfg(target_os = "linux")]
mod host;

use tilo_actions::ApiError;
use tilo_actions::remote::downloaded_apps as downloaded_wire;
use tilo_actions::remote::{
    ActionCallError, Caller, Found, RemoteActionTarget, ResourceSearch, application_open_target,
};
use tilo_desktop_search::preferences;
use tilo_desktop_search::providers::PublishedActions;
use tilo_desktop_search::providers::downloaded::Downloaded;
use tilo_desktop_search::providers::happening::{Happening, WorkspaceHere};
use tilo_desktop_search::providers::host_apps;
use tilo_desktop_search::providers::jobs::{JobEntry, Jobs};
use tilo_desktop_search::providers::people::People;
use tilo_desktop_search::providers::places::{Continue, Place, RecentPlaces};
use tilo_desktop_search::providers::records as record_rows;
use tilo_desktop_search::providers::settings::Settings;
use tilo_desktop_search::providers::waiting::Waiting;
use tilo_desktop_search::{
    DESKTOP_APP, Held, InvokeTarget, LaunchPlace, SearchPreferences, Source, Target, Usage,
};
use tilo_session_core::desktop::state::{SearchHost, SearchHostFeed};
use ui::resource::{DocumentCheckout, DocumentId, ResourceType};
use ui::search::Scope;

/// The action host as the session's search reaches it: what open apps publish and run, and running
/// what is chosen by address.
/// What hears an app action's answer ([`AppActions::invoke_answered`]): on whichever thread the
/// answer lands, what it answered (its value and the resources it made), or why it did not run.
pub type Answered = Box<dyn FnOnce(Result<tilo_actions::ActionOutput, String>) + Send>;

pub trait AppActions {
    /// What the apps publish through the action host, by app and workspace: those open now and
    /// those that closed after publishing, which a chosen row starts again. `workspace` narrows it
    /// to one workspace, and the empty string asks for every workspace's.
    fn published(&self, _workspace: &str) -> Vec<PublishedActions> {
        Vec::new()
    }
    /// The invocations the action host follows for the person.
    fn jobs(&self) -> Vec<JobEntry> {
        Vec::new()
    }
    /// Whether this host carries out `target` itself rather than leaving it to the shell.
    ///
    /// A host that starts windows carries out every row: it reaches an app's window whether the
    /// app is open or closed, and opens a launch where the person asked it. A host that arranges
    /// its own windows answers only the rows that reach an app's own window
    /// ([`Target::Invoke`]) — a page opens the app for those — and leaves the launches, the
    /// addresses and the stopped runs to the shell, which arranges them.
    fn performs(&self, _target: &Target) -> bool {
        true
    }
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
    ) -> Result<(), ApiError>;
    /// [`Self::invoke`], and tell `answered` what the action answered once it settled: its value,
    /// or why it failed. A host that hears no answers tells it that the action was taken, at once.
    ///
    /// # Errors
    /// The host's refusal.
    fn invoke_answered(
        &self,
        target: RemoteActionTarget,
        arguments: serde_json::Map<String, serde_json::Value>,
        answered: Answered,
    ) -> Result<(), ApiError> {
        let invoked = self.invoke(target, arguments);
        answered(
            invoked
                .as_ref()
                .map(|()| tilo_actions::ActionOutput::default())
                .map_err(ToString::to_string),
        );
        invoked
    }
    /// Open `item`, what the `open` `target` shows when run with `arguments`, in a window of the
    /// app's own beside the one it has: what the person asked by the modifier they held over its
    /// row. A host that starts no windows opens it where `open` does.
    ///
    /// # Errors
    /// The host's refusal.
    fn open_beside(
        &self,
        target: RemoteActionTarget,
        arguments: serde_json::Map<String, serde_json::Value>,
        item: &str,
    ) -> Result<(), ApiError> {
        let _ = item;
        self.invoke(target, arguments)
    }
    /// Ask the invocation `invocation` of `app` in `workspace` to stop.
    ///
    /// # Errors
    /// The host's refusal: no such invocation, or it cannot be stopped.
    fn cancel(&self, _app: &str, _workspace: &str, _invocation: &str) -> Result<(), ApiError> {
        Err(ApiError::Unavailable {
            code: "unknown_invocation".into(),
            reason: "This host runs no invocations".into(),
        })
    }
    /// Open `uri` in `workspace`, in the app that takes its scheme: a file,
    /// a Store page, a web address.
    ///
    /// # Errors
    /// Nothing takes it.
    fn open_uri(&self, _workspace: &str, _uri: &str) -> Result<(), ApiError> {
        Err(ApiError::Unavailable {
            code: "no_uri_opener".into(),
            reason: "This host opens no addresses".into(),
        })
    }
    /// Start the machine's own app `desktop_id`.
    ///
    /// # Errors
    /// It did not start.
    fn start_host_app(&self, _desktop_id: &str) -> Result<(), ApiError> {
        Err(ApiError::Unavailable {
            code: "no_host_apps".into(),
            reason: "This host runs no programs of the machine's".into(),
        })
    }

    /// Open the content whose hash is `hash`, called `name`, in the app its bytes say opens it: a
    /// file of a workspace's tree, which a host that holds the workspace's content carries out. A
    /// host with no such content refuses the row.
    ///
    /// # Errors
    /// It does not hold the content, or the app did not open.
    fn open_content(&self, _hash: &str, _name: &str) -> Result<(), ApiError> {
        Err(ApiError::Unavailable {
            code: "no_content".into(),
            reason: "This host holds no workspace content to open".into(),
        })
    }

    /// Open what a row of search asked for whose content was still on its way from the workspace,
    /// now that it arrived: the file the person chose opens without being chosen again. A desktop
    /// that holds content no further off than it reads has nothing to do here.
    fn open_arrived(&self) {}
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
    ) -> Result<(), ApiError> {
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
    /// What the person's files say, as this host indexes them; `None` where the host keeps no
    /// index.
    pub indexes: Option<Arc<dyn Indexes>>,
    /// The device's search index of what apps hold, as the device's daemon answers it; `None`
    /// where this host reaches none.
    pub index: Option<Arc<dyn ResourceSearch>>,
    /// The sources the person switched off.
    pub preferences: SearchPreferences,
    /// Where the person's windows were.
    pub places: RecentPlaces,
    /// The records the workspaces on this device hold: what search finds by name across them, and
    /// where a record brought here is named.
    pub records: Option<Records>,
    /// The files of the workspace in front, as a host that holds them without a disk keeps them
    /// ([`tilo_desktop_search::providers::tree`]): a page's Files are the workspace's own tree, so
    /// the page hands search that tree rather than an index of a disk it does not have.
    pub workspace_files: Option<Held>,
    /// What the workspace's host finds in what the workspace holds
    /// ([`tilo_desktop_search::providers::hosted`]), where this host holds only the records it
    /// opened: a page, whose store has none of a notebook it never showed.
    pub host_search: Option<Held>,
    /// Sources a test mounts beside the session's own, asked exactly as the host's are: an index
    /// that answers slowly stands here to prove the loop never waits for one. No production host
    /// sets it, and a search over no extra source is the one the device's own sources answer.
    pub extra_sources: Vec<Held>,
}

/// The sources the session keeps between searches, each indexed once per reading of the apps they
/// are built from.
struct Kept {
    /// The reading of the host's apps the sources below were built from.
    reading: Option<Arc<dyn Reading>>,
    host_apps: Option<Held>,
    content: Option<Held>,
    files: Option<Held>,
    /// Settings' pages, the store's catalogue and what the installed apps offer while closed, for
    /// the workspace they were built for.
    workspace: Option<ForWorkspace>,
}

/// What search keeps of one workspace: Settings' pages, the store's catalogue and what the
/// installed apps offer while closed.
#[derive(Clone)]
struct ForWorkspace {
    workspace: String,
    settings: Held,
    catalogue: Option<Held>,
    offered: Rc<[PublishedActions]>,
}

/// The programs the person downloaded, as the registry last read them, and the session's published
/// actions for them: the apps, the offers, and what a row whose file is gone runs.
#[derive(Default)]
struct DownloadedRows {
    reading: downloaded_wire::DownloadedApps,
    published: Vec<tilo_actions::ActionDescriptor>,
}

/// Why a row the person chose did not happen, in words they read: the session's own missing action
/// provider says what it means, rather than naming its code on the row.
#[must_use]
pub fn why_in_words(error: &ApiError) -> String {
    match error {
        ApiError::Unavailable { code, .. } if code == "action_provider_unavailable" => {
            tilo_session_core::desktop::i18n::t("answer-why-cannot-start")
        }
        other => other.to_string(),
    }
}

/// The session's part in the shell's search, mounted by every host that runs a shell: the sources
/// only a session has, and the rows a host carries out ([`tilo_session_core::desktop::state::SearchHost`]).
pub struct SessionSearch {
    /// The apps this host lists, where it lists any.
    apps_lists: Option<Arc<dyn Apps>>,
    /// The model gateway this session's own questions run on, where it runs one.
    models: Option<Arc<dyn crate::platform::Models>>,
    /// What apps hold, as the device's index answers this session's person.
    content: Option<Arc<tilo_desktop_search::indexed::IndexedContent>>,
    /// The sources the person switched off, as kept: what the next search leaves out, and what a
    /// switch from search changes.
    preferences: RefCell<SearchPreferences>,
    places: RefCell<RecentPlaces>,
    /// What the person chose before; `None` until they chose or it was restored.
    usage: RefCell<Option<Usage>>,
    /// The parts of the history changed since they were last kept: the choices, the places.
    unkept: Cell<(bool, bool)>,
    kept: RefCell<Kept>,
    /// The items this device's readings list, each by the resource whose reading lists it: what
    /// search finds whether or not the app showing them runs.
    device_rows: RefCell<Vec<(String, tilo_actions::ReadingRow)>>,
    apps: Option<Rc<dyn AppActions>>,
    /// The records the workspaces on this device hold.
    records: Option<Records>,
    /// The files of the workspace in front, where the host holds them without a disk.
    workspace_files: Option<Held>,
    /// The workspace host's search, where this host holds only part of the workspace.
    host_search: Option<Held>,
    /// The workspace's action catalogue, where this session serves one: the rows `tilo do` and a
    /// page's catalogue query read, so the person's search lists an app that runs on the
    /// workspace's host too ([`tilo_desktop_search::providers::HostActions`]).
    catalogue:
        RefCell<Option<std::sync::Arc<dyn tilo_actions::remote::ActionProvider + Send + Sync>>>,
    /// Sources a test mounts beside the session's own, asked as the host's are ([`SearchOptions::extra_sources`]).
    extra_sources: Vec<Held>,
    /// What the session itself publishes, as of the last frame: the shell cannot read its own tree
    /// while it opens search.
    own: RefCell<Vec<tilo_actions::ActionDescriptor>>,
    /// What the resources the session publishes are, by id, as of the last frame: what a query
    /// naming a type ("my mails") finds them by.
    own_types: RefCell<std::collections::BTreeMap<String, tilo_actions::ResourceType>>,
    /// The windows open, as of the last frame.
    open_tabs: RefCell<Vec<u64>>,
    /// The live notifications the session publishes, as of the last frame.
    notifications: RefCell<Vec<tilo_actions::ResourceSpec>>,
    /// The workspaces the shell shows, by id and the name it gives them: what a row of another
    /// workspace says.
    workspaces: RefCell<Vec<(String, String)>>,
    /// Who is in each workspace and the calls in progress, as the shell and the presence it holds
    /// say: what the empty palette's first group lists.
    here: RefCell<Vec<WorkspaceHere>>,
    /// The downloaded programs, as of the last reading of the registry.
    downloaded: RefCell<DownloadedRows>,
    /// The agent each workspace keeps for the person here, as the pass read them: whom the row
    /// that hands an agent what was typed sends to. `None` where this host sends nothing at all
    /// (the page), which offers no such row.
    #[cfg(target_os = "linux")]
    agents: RefCell<Option<Vec<crate::agents::Kept>>>,
    /// Every agent each workspace's record lists, whoever keeps it: whom a message can go to.
    listed_agents: RefCell<Vec<ListedAgent>>,
    /// Every person each workspace's governance lists, by the name it knows them by: who can be
    /// found in it before they are here.
    members: RefCell<Vec<(String, Vec<collab_types::Member>)>>,
    /// The person searching, by their root: whom search never offers to themselves.
    person: String,
    /// How often who is here or what the agents' records list moved: what a box that reads whom a
    /// message can go to is told again for.
    recipients_moved: Cell<u64>,
    /// The question search answered, and the answer as it is being written: kept across the builds
    /// of the rows so what arrives stays where the person asked it.
    ask: crate::search_ask::Shared,
    /// What wakes the session's loop as an answer is written, so the row shows it as it lands.
    wake: Option<tilo_service::Wake>,
    /// The sources' threads for a find with no frame of the toolkit's own: an API's `find` runs
    /// on the loop's thread and must never wait for a source, so its search asks these and the
    /// answers come on a later pass.
    workers: tilo_desktop_search::SearchWorkers,
    /// The finds still waiting for a source, each taken up on a later pass.
    pending: RefCell<Vec<PendingFind>>,
}

/// A find an API asked, as it stands between passes: the search's stream, the sources it was
/// made from (which outlive the stream's own borrow of them), and where the answer goes.
struct PendingFind {
    stream: tilo_desktop_search::SearchStream,
    /// The sources the stream was made from: moved with it, since the stream borrows them each
    /// pass it is asked.
    sources: Vec<Held>,
    /// The pool this find's sources work on: its own, since a find asked while another is in
    /// flight must not retire the other's answer (`SearchWorkers::fork`).
    workers: tilo_desktop_search::SearchWorkers,
    /// What the person's earlier choices add to a row, as they were when the find was asked.
    used: std::rc::Rc<dyn Fn(&tilo_desktop_search::Hit) -> f32>,
    needle: String,
    workspace: Option<String>,
    /// Where the answer goes, and the pass it must be answered by: the caller has its own
    /// deadline, and a source that never answers is cut off with what is in.
    answer: Option<FindAnswer>,
    due: ui::time::Instant,
}

/// Who a find was asked for, and so what its hits become when they are in.
pub(crate) enum FindAnswer {
    /// A API's find: the resources the hits name, as a caller who addresses them reads them,
    /// under what a place can show ([`found_all`]).
    Api {
        done: FoundSearch,
        /// What a place can show, read at the ask where the layout is: `'static` by holding
        /// the catalogue itself, not a borrow of it.
        shows: Rc<dyn Fn(&str) -> bool>,
    },
    /// Text typed on the canvas: the hits in the ranking's order, which the layout places
    /// through its own catalogue (never a second reading of what a place can show).
    Canvas(tilo_session_core::layout::FindDone),
}

/// Answer `answer` with the hits a search has: an API's find as the resources they name, the
/// canvas's as the hits themselves. `needle` is what was typed, which [`found_all`] ranks the
/// lead by.
fn finish(answer: FindAnswer, needle: &str, hits: &[tilo_desktop_search::Hit]) {
    match answer {
        FindAnswer::Api { done, shows } => {
            let shows = &*shows;
            done.complete(found_all(needle, hits.to_vec(), shows));
        }
        FindAnswer::Canvas(done) => done(hits.to_vec()),
    }
}

/// What an app is called here, as the host's own lists name it: the answer a row says an app by.
fn names(apps: Option<Arc<dyn Apps>>) -> impl Fn(&str) -> String + Send + Sync + 'static {
    move |app: &str| match &apps {
        Some(apps) => apps.name(app),
        None => tilo_desktop_search::names::app_name(app),
    }
}

/// The resource of the section keeping search's history.
pub(crate) fn history() -> String {
    tilo_app::framework::section_address(settings_store::search_history::SECTION, None)
}

/// One field of the history, set whole: its setter, and the setter's arguments.
pub type HistorySet = (String, serde_json::Map<String, serde_json::Value>);

/// The setter of the history's field `field`, setting it to `value`.
fn set_field(field: &str, value: &serde_json::Value) -> HistorySet {
    let mut arguments = serde_json::Map::new();
    arguments.insert(tilo_app::framework::SETTER_VALUE.to_owned(), value.clone());
    (ui::setting_verb(field), arguments)
}

impl SessionSearch {
    /// The session's search over what `options` names the host offers it, as `person` —
    /// answering a question the person types through `wake` and `models`, where the host runs a
    /// gateway.
    #[must_use]
    pub fn new(
        options: SearchOptions,
        lists: Option<Arc<dyn Apps>>,
        person: &str,
        apps: Option<Rc<dyn AppActions>>,
        wake: Option<tilo_service::Wake>,
        models: Option<Arc<dyn crate::platform::Models>>,
    ) -> Self {
        let SearchOptions {
            indexes,
            index,
            preferences,
            places,
            records,
            workspace_files,
            host_search,
            extra_sources,
        } = options;
        let reading = lists.as_ref().map(|lists| lists.reading());
        let indexed = indexes.as_ref().map(|indexes| indexes.of(person));
        // Rows about what an app holds say who wrote them: the person at this session.
        let content = index.map(|index| {
            let wake = wake.clone().unwrap_or_else(|| Arc::new(|| {}));
            Arc::new(tilo_desktop_search::indexed::IndexedContent::new(
                index,
                Caller::local_person(person),
                crate::app_names::app_name,
                person,
                wake,
            ))
        });
        let kept = Kept {
            host_apps: reading
                .as_ref()
                .and_then(|read| read.host_apps(&preferences)),
            reading,
            content: content.clone().map(Held::from),
            files: indexed.as_ref().and_then(|index| index.files()),
            workspace: None,
        };
        // The sources of an API's `find` answer on their own threads, and the wake asks the loop
        // for the pass that takes them up: the same wake the session's services use.
        let workers = match &wake {
            Some(wake) => tilo_desktop_search::SearchWorkers::waking(Arc::clone(wake)),
            None => tilo_desktop_search::SearchWorkers::waking(Arc::new(|| {})),
        };
        Self {
            apps_lists: lists,
            preferences: RefCell::new(preferences),
            places: RefCell::new(places),
            usage: RefCell::new(None),
            unkept: Cell::new((false, false)),
            kept: RefCell::new(kept),
            device_rows: RefCell::default(),
            models,
            content,
            apps,
            records,
            workspace_files,
            host_search,
            catalogue: RefCell::default(),
            extra_sources,
            own: RefCell::default(),
            own_types: RefCell::default(),
            open_tabs: RefCell::default(),
            notifications: RefCell::default(),
            workspaces: RefCell::default(),
            here: RefCell::default(),
            downloaded: RefCell::default(),
            #[cfg(target_os = "linux")]
            agents: RefCell::default(),
            listed_agents: RefCell::default(),
            members: RefCell::default(),
            person: person.to_owned(),
            recipients_moved: Cell::default(),
            ask: crate::search_ask::Shared::default(),
            workers,
            pending: RefCell::default(),
            wake,
        }
    }

    /// Serve the workspace's action catalogue as a source of the person's search: `provider` is
    /// this session's action host, the one `tilo do` and an agent's tools ask.
    /// Its rows are listed for the workspace in front, less the ones this device's own sources
    /// already publish ([`tilo_desktop_search::providers::HostActions`]).
    pub fn serve_catalogue(
        &self,
        provider: std::sync::Arc<dyn tilo_actions::remote::ActionProvider + Send + Sync>,
    ) {
        *self.catalogue.borrow_mut() = Some(provider);
    }

    /// What the session publishes now, the windows open and the downloaded programs as the registry
    /// reads them: read after each frame, outside the desktop's trees.
    pub fn observe(
        &self,
        own: &[tilo_actions::ResourceSpec],
        open_tabs: Vec<u64>,
        downloaded: Option<downloaded_wire::DownloadedApps>,
        notifications: Vec<tilo_actions::ResourceSpec>,
    ) {
        *self.notifications.borrow_mut() = notifications;
        let mut published = tilo_actions::api::survey(own);
        // Search does not list itself, nor opening itself.
        published.retain(|action| {
            let address = &action.address;
            let searching = address
                .resource
                .starts_with(tilo_session_core::desktop::verbs::SEARCH_RESOURCE);
            let opening = address.resource == tilo_session_core::desktop::verbs::SHELL_RESOURCE
                && address.action == "search";
            !searching && !opening
        });
        // The chrome's resource is named after the desktop, not after the thing an action acts on:
        // matched as a word, that name would list every one of its actions.
        for action in &mut published {
            if action.address.resource == tilo_desktop_search::CHROME_RESOURCE {
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

    /// Who is in the person's workspaces now, as their presence and the shell's members say
    /// it: the empty palette's first group.
    pub fn set_here(&self, workspaces: Vec<WorkspaceHere>) {
        if *self.here.borrow() == workspaces {
            return;
        }
        *self.here.borrow_mut() = workspaces;
        self.moved_recipients();
    }

    /// The items this device's readings list, each by the resource whose reading lists it: a
    /// container, a drive, a printer, a process, a journal record. Search finds them whether or
    /// not the app that shows them runs, and the source turns off with the others.
    pub fn set_device_rows(&self, rows: Vec<(String, tilo_actions::ReadingRow)>) {
        if *self.device_rows.borrow() == rows {
            return;
        }
        *self.device_rows.borrow_mut() = rows;
    }

    /// This device's readings as a source of search: each item of each reading, opened the way
    /// its reading offers. None where the person turned the source off.
    fn device_source(
        &self,
        workspace: &str,
        rows: Vec<(String, tilo_actions::ReadingRow)>,
    ) -> Option<Held> {
        if rows.is_empty()
            || !self
                .preferences
                .borrow()
                .enabled(tilo_desktop_search::providers::device_rows::DEVICE_ROWS_SOURCE)
        {
            return None;
        }
        Some(Held::new(
            tilo_desktop_search::providers::device_rows::DeviceRows::new(workspace, rows),
        ))
    }

    /// Say that who a message can go to moved, so the reading of it is taken again.
    fn moved_recipients(&self) {
        self.recipients_moved
            .set(self.recipients_moved.get().wrapping_add(1));
    }

    /// The agent each workspace keeps for the person here, as the pass read them: whom the row
    /// that hands an agent what was typed sends to, and a new one where a workspace keeps none.
    #[cfg(target_os = "linux")]
    pub(crate) fn set_kept_agents(&self, agents: &[crate::agents::Kept]) {
        *self.agents.borrow_mut() = Some(agents.to_vec());
    }

    /// The agent `workspace` keeps for the person here, by its id and name, as the pass read it:
    /// whom a send goes to, and a new one where the workspace keeps none.
    #[cfg(target_os = "linux")]
    pub(crate) fn kept_agent(&self, workspace: &str) -> Option<(String, String)> {
        self.agents
            .borrow()
            .as_ref()?
            .iter()
            .find(|agent| agent.workspace == workspace)
            .map(|agent| (agent.id.clone(), agent.name.clone()))
    }

    /// Record every agent each workspace's record lists, whoever keeps it: what a box that sends
    /// a message addresses besides the people present and what the apps published.
    pub(crate) fn set_listed_agents(&self, agents: &[ListedAgent]) {
        if *self.listed_agents.borrow() == agents {
            return;
        }
        *self.listed_agents.borrow_mut() = agents.to_vec();
        self.moved_recipients();
    }

    /// Keep the people each workspace's governance lists, as the engine admitted them.
    pub(crate) fn set_members(&self, members: Vec<(String, Vec<collab_types::Member>)>) {
        if *self.members.borrow() == members {
            return;
        }
        *self.members.borrow_mut() = members;
        self.moved_recipients();
    }

    /// What the workspaces' lists of agents call the agent `id`, whether it works or was retired:
    /// a conversation or a mention names it by the name its workspace knows.
    pub(crate) fn agent_name(&self, id: &str) -> Option<String> {
        self.listed_agents
            .borrow()
            .iter()
            .find(|agent| agent.id == id)
            .map(|agent| agent.name.clone())
    }

    /// The workspaces this device shows, by id and the name the shell gives it: what a row says of a
    /// record the workspace in front does not hold.
    pub fn set_workspaces(&self, workspaces: Vec<(String, String)>) {
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
        let host_apps = reading.as_ref().and_then(|read| read.host_apps(&next));
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
        // A window's search is about that window: what its app published into this session's
        // index, and the actions the app publishes, and none of the shell's own rows (its other
        // windows, the places it was, the apps it can start).
        if let Scope::InApp(app) = scope {
            return self.window_sources(workspace, app);
        }
        let now = ui::time::unix_now();
        let ForWorkspace {
            settings,
            catalogue,
            offered,
            ..
        } = self.for_workspace(workspace);
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
            Held::new(People::new(workspace, self.recipients(workspace))),
        ];
        sources.push(settings);
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
        sources.extend(self.device_source(workspace, self.device_rows.borrow().clone()));
        sources.extend(kept.files.clone());
        sources.extend(self.workspace_files.clone());
        sources.extend(self.host_search.clone());
        if let Some(records) = &self.records {
            sources.push(Held::new(record_rows::Records::new(
                workspace,
                records.held(workspace),
            )));
        }
        sources.extend(self.extra_sources.iter().cloned());
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
        // An installed app never opened here publishes nothing yet: what its manifest offers
        // while closed stands in, and choosing one opens the app and invokes it there.
        let published =
            tilo_desktop_search::providers::with_offers(published, offered.iter().cloned());
        // What the workspace's action catalogue offers that this device's own sources do not:
        // the actions of an app running on the workspace's host, and of a record the workspace
        // keeps for all its apps, so the person finds on their screen what `tilo do` finds.
        // Rows are ranked in this workspace, whichever one the action itself belongs to.
        if !scope.everywhere()
            && let Some(catalogue) = self.catalogue.borrow().clone()
        {
            let listed: std::collections::HashSet<tilo_actions::ActionAddress> = published
                .iter()
                .flat_map(|published| published.actions.iter())
                .map(|action| action.address.clone())
                .collect();
            let silent: std::sync::Arc<dyn Fn() + Send + Sync> = std::sync::Arc::new(|| {});
            let wake = self.wake.clone().unwrap_or(silent);
            let caller = tilo_actions::remote::Caller::local_person(self.person.as_str())
                .in_workspace(workspace);
            sources.push(Held::new(tilo_desktop_search::providers::HostActions::new(
                catalogue,
                caller,
                workspace,
                std::sync::Arc::new(listed),
                wake,
            )));
        }
        (sources, published)
    }

    /// What one window's search finds: what its app published into this session's index, as a
    /// source over that app alone, and the actions the app publishes in `workspace`. An app that
    /// runs in a process of its own hands this session no widget tree, so what its window holds is
    /// read from the index it publishes instead.
    fn window_sources(
        &self,
        workspace: &str,
        app: &ui::search::AppScope,
    ) -> (Vec<Held>, Vec<PublishedActions>) {
        let mut sources = Vec::new();
        if let Some(name) = app.app.as_deref()
            && let Some(content) = &self.content
        {
            sources.push(Held::new(
                content.for_app(name, app.part.as_ref().map(|part| part.anchor.clone())),
            ));
        }
        // A window's own search finds the items of the readings its app shows, which its record
        // publishes none of.
        if let Some(name) = app.app.as_deref() {
            let rows: Vec<(String, tilo_actions::ReadingRow)> = self
                .device_rows
                .borrow()
                .iter()
                .filter(|(_, row)| row.app.as_deref() == Some(name))
                .cloned()
                .collect();
            sources.extend(self.device_source(workspace, rows));
        }
        let published = self
            .apps
            .as_ref()
            .map(|apps| apps.published(workspace))
            .unwrap_or_default()
            .into_iter()
            .filter(|published| app.app.as_deref() == Some(published.app.as_str()))
            .collect();
        (sources, published)
    }

    /// The results of one search over the sources a query that names a resource asks, in the
    /// ranking's order: what text typed on the canvas places (shell.md §8). Every one, as ranked:
    /// the best resource to place may stand below many actions the text names ("Inbox" under each
    /// conversation's "Move to inbox").
    ///
    /// Test code only: a session's own caller must never wait for a source on the loop it draws
    /// from, so the production build has no such entry at all, and its finds ask with
    /// [`Self::find_soon`] and take the answer on a later pass. The one waiting search left is
    /// `tilo_desktop_search::search`'s, which a test drives on its own thread.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn find(&self, needle: &str) -> Vec<tilo_desktop_search::Hit> {
        self.find_in(needle, None)
    }

    /// [`Self::find`] for a caller confined to `workspace` (an agent): what that workspace's apps
    /// publish and its records hold, and none of the shell's own rows, whose windows and places
    /// are the person's across every workspace (a window's title names what another workspace
    /// holds). `None` is the person's search, over everything they see.
    ///
    /// Test code only, as [`Self::find`] is.
    #[cfg(test)]
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

    /// What [`Self::find_in`] answers, asked by the session's own API on the loop's thread: the
    /// sources are handed to their workers and the search stays pending (`shows` names what a
    /// place can show, read here where the layout is), and `done` hears what it found on a
    /// later pass ([`Self::poll_finds`]), as its answers land.
    ///
    /// A source that never answers cuts the find off at `within` with the rows that are in,
    /// saying so: the caller's own deadline, never the loop's. The loop keeps running at its own
    /// rate while a search is in flight, which is the point: an MCP `find` over the MCP server
    /// must not freeze the desktop's frames.
    pub(crate) fn find_soon(
        &self,
        needle: &str,
        workspace: Option<&str>,
        shows: Rc<dyn Fn(&str) -> bool>,
        within: std::time::Duration,
        done: FoundSearch,
    ) {
        self.find_off_loop(needle, workspace, within, FindAnswer::Api { done, shows });
    }

    /// Text typed on the canvas, asked of the same search off the pass that places it: the hits
    /// in the ranking's order reach `done` on a later pass, which places what they name. Never
    /// waits for a source, so a canvas whose index is reading a disk keeps its frames.
    pub(crate) fn place_soon(
        &self,
        needle: &str,
        workspace: Option<&str>,
        within: std::time::Duration,
        done: tilo_session_core::layout::FindDone,
    ) {
        self.find_off_loop(needle, workspace, within, FindAnswer::Canvas(done));
    }

    /// The one search a host's find runs: the sources handed to their workers, and `answer` heard
    /// on a later pass.
    fn find_off_loop(
        &self,
        needle: &str,
        workspace: Option<&str>,
        within: std::time::Duration,
        answer: FindAnswer,
    ) {
        let needle = needle.trim();
        if needle.is_empty() {
            finish(answer, needle, &[]);
            return;
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
        let now = ui::time::unix_now();
        let usage = self.usage.borrow().clone();
        // The find's own pool, so a find asked while this one is in flight does not retire it: a
        // palette's newer query replaces the older, but two finds of a host's are each answered.
        let workers = self.workers.fork();
        let typed = needle.to_owned();
        let used: Rc<dyn Fn(&tilo_desktop_search::Hit) -> f32> = {
            let typed = typed.clone();
            Rc::new(move |hit: &tilo_desktop_search::Hit| match usage.as_ref() {
                Some(usage) => usage.strength(&typed, hit, now),
                None => 0.0,
            })
        };
        let stream = tilo_desktop_search::search_stream(needle, &refs, 0, &workers, &*used);
        // What is in already answers at once; what still works comes on a later pass.
        if stream.pending() {
            let due = ui::time::now()
                .checked_add(within)
                .unwrap_or_else(ui::time::now);
            self.pending.borrow_mut().push(PendingFind {
                stream,
                sources,
                workers,
                used,
                needle: typed,
                workspace: workspace.map(str::to_owned),
                answer: Some(answer),
                due,
            });
            return;
        }
        finish(answer, needle, &stream.rows());
    }

    /// Take up the finds whose answers are in, and those a source did not answer in time: called
    /// once per pass, and what asks the loop for that pass (`workers`'s wake is the loop's own).
    pub(crate) fn poll_finds(&self) {
        let waiting = self.pending.take();
        if waiting.is_empty() {
            return;
        }
        let mut kept = Vec::with_capacity(waiting.len());
        for mut find in waiting {
            // The sources that stay on the frame are asked one per pass here, as a palette's
            // frame asks them, and the answers the workers sent are merged: never a wait.
            let refs: Vec<&dyn Source> = find
                .sources
                .iter()
                .map(|source| source as &dyn Source)
                .collect();
            let _ = find.stream.resume(&refs, &find.workers, &*find.used);
            let overdue = ui::time::now() >= find.due;
            if find.stream.pending() && !overdue {
                kept.push(find);
                continue;
            }
            let Some(answer) = find.answer.take() else {
                continue;
            };
            if overdue && find.stream.pending() {
                tracing::warn!(
                    needle = %find.needle,
                    workspace = ?find.workspace,
                    "a find answers with the rows that came: a source did not answer in time"
                );
            }
            finish(answer, &find.needle, &find.stream.rows());
        }
        *self.pending.borrow_mut() = kept;
    }

    /// The app and the resource kind `needle` names, when it names a type of an app whose origin
    /// the query could ask for what it says (`outside:<origin>.search`, shell.md §8, search.md
    /// §7): the words of the query that are a type's noun or plural, or its app's name, choose the
    /// type, exactly as the one search ranks by what a resource is. `None` when no word of the
    /// query names a type, or the type's app holds no origin the platform can ask.
    ///
    /// The app is named by the id its resources publish under (`so.tilo.tube`), and the kind
    /// as its app publishes it (`tube.channel`): the origin's own search action is the one that
    /// asks the service for it, and an answer of that kind is what a place becomes.
    #[must_use]
    pub(crate) fn origin_search(&self, needle: &str) -> Option<(String, String, String)> {
        let needle = ui::search::system::without_filler(needle.trim());
        if needle.trim().is_empty() {
            return None;
        }
        // The session's own published actions, from its API: the apps' and the seeded records'
        // alike, which is where an app's search action and its types are (the palette's `own`).
        let own = self.own.borrow();
        let types = self.own_types.borrow();
        // The app each of the session's resources belongs to, by its id in the app's own address
        // (`tl:so.tilo.tube:…` → `so.tilo.tube`).
        let app_of = |resource: &str| {
            resource
                .strip_prefix("tl:")
                .and_then(|rest| rest.split_once(':'))
                .map(|(app, _)| app.to_owned())
        };
        // The nouns of every type each app publishes, by app: a word naming one is about that
        // app's things, so "youtube videos of <name>" is its `Channel` and not its `Video`.
        let mut kin: std::collections::BTreeMap<String, Vec<&tilo_actions::Noun>> =
            std::collections::BTreeMap::new();
        for (resource, named) in types.iter() {
            let Some(app) = app_of(resource) else {
                continue;
            };
            let nouns = kin.entry(app).or_default();
            if !nouns.contains(&&named.noun) {
                nouns.push(&named.noun);
            }
        }
        // The apps that answer a query from a service: those that ask an outside origin
        // (`outside:<origin>.search`), and those that declare the action themselves
        // (`search: Found`), whose answers are of the kind their tag names.
        let searchers: std::collections::BTreeSet<String> = own
            .iter()
            .filter(|action| {
                action
                    .contract
                    .outside_capabilities()
                    .any(|capability| capability.ends_with(".search"))
                    || action
                        .contract
                        .tags
                        .iter()
                        .any(|tag| tag == tilo_actions::SEARCH_TAG)
            })
            .filter_map(|action| app_of(action.address.resource.as_str()))
            .collect();
        // The kind each app's declared search answers with, where it declares one.
        let declared: std::collections::BTreeMap<String, String> = own
            .iter()
            .filter_map(|action| {
                let app = app_of(action.address.resource.as_str())?;
                let kind = action
                    .contract
                    .tags
                    .iter()
                    .find_map(|tag| tag.strip_prefix(tilo_actions::SEARCH_KIND_PREFIX))?;
                Some((app, kind.to_owned()))
            })
            .collect();
        let mut found: Vec<(usize, (String, String, String, bool))> = Vec::new();
        for (resource, named) in types.iter() {
            if named.root {
                // A root names the app rather than a type of its own.
                continue;
            }
            let Some(app) = app_of(resource) else {
                continue;
            };
            if !searchers.contains(&app) {
                continue;
            }
            // The kind the type's resources publish under (`tl.<app>.<Type>`): what a query
            // names, and what an answer of that kind becomes a place of.
            // Where the type is first published among the app's actions: the order its package
            // declares its types in, which the tie between types a query names alike falls back
            // on, so the same words name the same type on every run.
            let Some((order, kind)) = own
                .iter()
                .position(|action| action.address.resource.as_str() == resource.as_str())
                .and_then(|at| own.get(at).map(|action| (at, action.resource_kind.clone())))
            else {
                continue;
            };
            // An app that declares its search answers with one kind alone: a query naming its
            // kind asks it, and its other types are not what it finds.
            if declared.get(&app).is_some_and(|declared| *declared != kind) {
                continue;
            }
            // Does any word of the query name this type? The same rule the ranking uses: a noun
            // or plural of it, or the app's name, with the rest sent to the app.
            let keywords = tilo_desktop_search::availability::keywords_for(&app);
            let rest = ui::search::system::type_words_rest(
                &needle,
                named,
                kin.get(&app)
                    .into_iter()
                    .flatten()
                    .copied()
                    .filter(|noun| **noun != named.noun),
                keywords,
            );
            // Whether the query names it by its own noun ("videos") or only through its app or a
            // kindred type's noun ("youtube videos of <channel>"): the type the person means is
            // the one the other words name, not the one whose noun they typed to narrow it.
            let by_own_noun = ui::search::system::named_by_own_noun(&needle, named);
            if let Some(rest) = rest {
                found.push((order, (app.clone(), kind, rest, by_own_noun)));
            }
        }
        // One app and one kind. A type named only through its app or a kindred type ("of <channel>"
        // after "videos") is the one the query means over one named by its own noun ("videos").
        found.sort_by_key(|(order, _)| *order);
        let found: Vec<(String, String, String, bool)> =
            found.into_iter().map(|(_, named)| named).collect();
        let narrowed: Vec<&(String, String, String, bool)> =
            found.iter().filter(|(.., by_noun)| !by_noun).collect();
        let candidates: Vec<&(String, String, String, bool)> = if narrowed.is_empty() {
            found.iter().collect()
        } else {
            narrowed
        };
        let (app, kind, rest, _) = candidates.first()?;
        if candidates.iter().any(|(other, ..)| *other != *app) {
            return None;
        }
        Some((app.clone(), kind.clone(), rest.clone()))
    }

    /// A window was at `place` .
    pub fn record_place(&self, place: Place) {
        let mut places = self.places.borrow_mut();
        let before = places.places().first().cloned();
        places.record(place);
        if places.places().first() != before.as_ref() {
            let (chosen, _) = self.unkept.get();
            self.unkept.set((chosen, true));
        }
    }

    /// Take up the history as the search history section ([`history`]) keeps it.
    pub fn restore(&self, kept: &serde_json::Value) {
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

    /// The setters that keep what changed of the history since it was last kept, each of a field
    /// of the search history section.
    pub fn take_unkept(&self) -> Vec<HistorySet> {
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

    /// Who can be found in `workspace`, the one list desktop search and every box that sends a
    /// message read: what its apps published (agents, people, conversations; the most recent
    /// first), the people present there, every person its governance lists and every agent it
    /// lists. Each member once, by kind and id, never the person searching, named by the
    /// one rule every surface names a member by ([`ui::presence::person_label`]).
    pub(crate) fn recipients(&self, workspace: &str) -> Vec<ui::floating_surface::Recipient> {
        use ui::floating_surface::Recipient;
        use ui::floating_surface::RecipientKind::{Agent, Conversation, Person};
        let members = self.members.borrow();
        let governed = members
            .iter()
            .filter(|(held, _)| held == workspace)
            .flat_map(|(_, people)| people)
            .filter(|member| !member.removed);
        let here = self.here.borrow();
        let listed = self.listed_agents.borrow();
        // Who is here first: the name a person chose for themselves comes with their presence.
        let members = here
            .iter()
            .filter(|here| here.workspace == workspace)
            .flat_map(|here| &here.people)
            .map(|person| (person.agent, person.id.as_str(), person.name.as_str()))
            .chain(
                governed
                    .clone()
                    .map(|member| (!member.id.is_person(), member.id.id(), member.name.as_str())),
            )
            .chain(
                listed
                    .iter()
                    .filter(|agent| agent.workspace == workspace && !agent.retired)
                    .map(|agent| (true, agent.id.as_str(), agent.name.as_str())),
            )
            .map(|(agent, id, name)| {
                Recipient::found(if agent { Agent } else { Person }, id, name)
            });
        let published = self
            .content
            .as_ref()
            .map_or_else(Vec::new, |content| content.recipients());
        let mut out: Vec<Recipient> = Vec::new();
        for recipient in published.into_iter().chain(members) {
            if recipient.kind == Person && recipient.id == self.person {
                continue;
            }
            match out
                .iter_mut()
                .find(|kept| kept.kind == recipient.kind && kept.id == recipient.id)
            {
                Some(kept) if ui::presence::shown_name(&kept.name).is_none() => {
                    kept.name = recipient.name;
                }
                Some(_) => {}
                None => out.push(recipient),
            }
        }
        for recipient in out.iter_mut().filter(|r| r.kind != Conversation) {
            recipient.name = ui::presence::person_label(&recipient.name);
        }
        out
    }

    /// A number that moves whenever what [`Self::recipients`] lists may have: what the apps
    /// published, and who is here or kept.
    pub(crate) fn recipients_generation(&self) -> (u64, u64) {
        (
            self.content
                .as_ref()
                .map_or(0, |content| content.generation()),
            self.recipients_moved.get(),
        )
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
                .and_then(|read| read.host_apps(&self.preferences.borrow()));
            kept.workspace = None;
            kept.reading.clone_from(&latest);
        }
        latest
    }

    /// Settings' pages and the catalogue for `workspace` , built once per workspace and reading of
    /// the host's apps.
    ///
    /// Every session runs Settings, so every search over one offers its pages: a computer's and a
    /// page's alike, each listing the pages it has ([`settings_store::index::Page::here`]). The
    /// settings the session publishes as actions that change them where they stand are left to
    /// those actions.
    fn for_workspace(&self, workspace: &str) -> ForWorkspace {
        let reading = self.current_reading();
        let mut kept = self.kept.borrow_mut();
        if let Some(built) = &kept.workspace
            && built.workspace == workspace
        {
            return built.clone();
        }
        // What a setting people look for by name is set to is an action of its own, published by
        // the settings service under the setting's own resource: those rows are the change, so the
        // page is not offered beside them.
        let settings = Held::new(Settings::of_published(workspace, &self.own.borrow()));
        let catalogue = reading.as_ref().and_then(|read| read.catalogue(workspace));
        let offered: Rc<[PublishedActions]> = reading
            .as_ref()
            .map(|read| read.offered(workspace))
            .unwrap_or_default()
            .into();
        let built = ForWorkspace {
            workspace: workspace.to_owned(),
            settings,
            catalogue,
            offered,
        };
        kept.workspace = Some(built.clone());
        built
    }

    /// Carry out `target`, an app's own action, and tell `answered` what it answered once it
    /// settled ([`AppActions::invoke_answered`]): what a caller that needs the action's answer —
    /// the message a send posted — runs instead of [`Self::perform`].
    ///
    /// # Errors
    /// `target` is no app's action, or the host refused it.
    pub(crate) fn perform_answered(
        &self,
        workspace: &str,
        target: Target,
        answered: Answered,
    ) -> Result<(), ApiError> {
        let _ = workspace;
        let Target::Invoke(invoke) = target else {
            return Err(ApiError::InvalidArguments(
                "only an app's action answers what it did".into(),
            ));
        };
        self.with_apps(|apps| apps.invoke_answered(invoke.remote(), invoke.arguments, answered))
    }

    /// Carry out the rows the person chose in `handle`'s palette since the last call, naming the
    /// workspace they were asked in, and return each row that could not be carried out with why.
    pub fn perform_choices(
        &self,
        handle: &tilo_session_core::desktop::state::DesktopHandle,
        own: &mut dyn FnMut(&InvokeTarget) -> Result<(), ApiError>,
    ) -> Vec<(Target, ApiError)> {
        let workspace = handle.read(|tree| {
            tree.active_workspace()
                .map(|held| held.id().to_string())
                .unwrap_or_default()
        });
        // What a row opened whose content was still on its way from the workspace opens the pass
        // it arrives, without the person choosing it again.
        if let Some(apps) = &self.apps {
            apps.open_arrived();
        }
        let mut refused = Vec::new();
        for choice in handle.take_search_choices() {
            if let Err(error) = self.perform(&workspace, choice.clone(), own) {
                refused.push((choice, error));
            }
        }
        refused
    }

    /// Carry out `target` , chosen in search in `workspace` : through the action host, or for the
    /// session's own actions through `own` , the session's published surface.
    pub fn perform(
        &self,
        workspace: &str,
        target: Target,
        own: &mut dyn FnMut(&InvokeTarget) -> Result<(), ApiError>,
    ) -> Result<(), ApiError> {
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
                let target = invoke.remote();
                if invoke.place != LaunchPlace::Beside {
                    return apps.invoke(target, invoke.arguments);
                }
                // The item an `open` shows: the one it is told, else the resource it is of.
                let item = invoke
                    .arguments
                    .get(tilo_desktop_search::OPEN_ANCHOR)
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(invoke.address.resource.as_str())
                    .to_owned();
                apps.open_beside(target, invoke.arguments, &item)
            }),
            // The content a page's files are, opened in the app its bytes say reads it: a host
            // that keeps no content itself refuses the row.
            Target::Content { hash, name } => {
                self.with_apps(|apps| apps.open_content(&hash, &name))
            }
            Target::Launch { app, place } => self.with_apps(|apps| {
                // A record this workspace holds opens as a link to it does: its app's `open` on
                // the record, which brings the window on it forward or opens its app there.
                if let Some((app, record)) = tilo_desktop_search::launched_record(&app) {
                    let open = InvokeTarget::place(
                        workspace,
                        &crate::platform::notice::Place {
                            app: app.to_owned(),
                            record: record.to_owned(),
                            ..Default::default()
                        },
                    );
                    return apps.invoke(open.remote(), open.arguments);
                }
                match host_apps::launched(&app) {
                    // The machine's own app places its window itself.
                    Some(desktop_id) => apps.start_host_app(desktop_id),
                    None => apps.open_application(workspace, &app, place),
                }
            }),
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
                tracing::debug!(?other, "search row the shell carries out");
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
    fn bring_here(&self, workspace: &str, invoke: &InvokeTarget) -> Result<(), ApiError> {
        let Some(records) = &self.records else {
            return Err(ApiError::Unavailable {
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
            return Err(ApiError::InvalidArguments(
                "a record row carries its id, its kind and its index".into(),
            ));
        };
        let Some(id) = DocumentId::parse(&id) else {
            return Err(ApiError::InvalidArguments(format!(
                "{id} is not a record id"
            )));
        };
        // Rows are collected when search opens and chosen afterwards, and an app may have stopped
        // naming the record in between: a row that names a record no workspace holds any more is
        // refused rather than written, so search never brings a record that is gone into a
        // workspace as if it were still there.
        if !records.holds(id.as_str()) {
            return Err(ApiError::Unavailable {
                code: "record_not_held".into(),
                reason: format!("No workspace holds {id} any more"),
            });
        }
        let path = id.file_path();
        if !records.bring_here(
            workspace,
            &package,
            &DocumentCheckout::new(id, ResourceType::new(kind)),
        ) {
            return Err(ApiError::Unavailable {
                code: "records_unavailable".into(),
                reason: "Records of another workspace are not brought here".into(),
            });
        }
        // A record kept as a file is opened as well, as the document it is: `file:/…` is a record
        // id, not a link, so it is the path the app is given. It opens after the reference is
        // named, so a file that is gone leaves the record held and says so.
        let Some(path) = path else {
            return Ok(());
        };
        if !path.exists() {
            return Err(ApiError::Failed(format!("{} is gone", path.display())));
        }
        let uri = tilo_desktop_search::file_uri(&path);
        self.with_apps(|apps| apps.open_uri(workspace, &uri))
    }

    fn with_apps(
        &self,
        run: impl FnOnce(&dyn AppActions) -> Result<(), ApiError>,
    ) -> Result<(), ApiError> {
        match &self.apps {
            Some(apps) => run(&**apps),
            None => Err(ApiError::Unavailable {
                code: "action_provider_unavailable".into(),
                reason: "The session runs without an action host".into(),
            }),
        }
    }
}

impl SessionSearch {
    /// The apps this session offers: the host's installed and machine apps, each by the launch its
    /// own row starts. A window group's new tab and the empty shell's launchpad search the
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
                    category: app.kind_of_app(),
                }
            }))
            .collect()
    }
}

impl SessionSearch {
    /// The apps installed here as `reading` lists them, which the palette offers as an app's row
    /// of its own: the machine's own apps are one of the session's sources instead.
    fn installed_applications(
        reading: Option<&Arc<dyn Reading>>,
    ) -> Vec<tilo_desktop_search::providers::AppEntry> {
        reading
            .map(|reading| reading.applications())
            .unwrap_or_default()
    }
}

impl SearchHost for SessionSearch {
    fn applications(&self) -> Vec<tilo_desktop_search::providers::AppEntry> {
        Self::applications(self)
    }

    fn feed(&self, workspace: &str, scope: &Scope) -> SearchHostFeed {
        feed(self, workspace, scope)
    }

    fn chose(&self, usage: &Usage) {
        *self.usage.borrow_mut() = Some(usage.clone());
        let (_, places) = self.unkept.get();
        self.unkept.set((true, places));
    }

    fn performs(&self, target: &Target) -> bool {
        match target {
            // The shell's own actions are the session's to run, by address.
            Target::Invoke(invoke) if invoke.app == DESKTOP_APP => true,
            Target::Invoke(_)
            | Target::Content { .. }
            | Target::Launch { .. }
            | Target::Cancel { .. }
            | Target::Open { .. } => self.apps.as_ref().is_some_and(|apps| apps.performs(target)),
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

/// What a search over `workspace` finds beyond the desktop's own: the session's sources over what
/// it keeps and what is published ([`SessionSearch::sources`]), its question to a model, and its
/// send to an agent.
fn feed(search: &SessionSearch, workspace: &str, scope: &Scope) -> SearchHostFeed {
    let (mut sources, published) = search.sources(workspace, scope);
    // A question typed by the person is answered here, by the model their AI settings choose:
    // never by a tool that searches. A host that runs no gateway asks nothing, and its palette
    // offers no question. A window's search is about the window: what it holds and what can be
    // done in it, never a question of its own.
    if search.models.is_some() && !matches!(scope, Scope::InApp(_)) {
        sources.push(Held::new(crate::search_ask::Ask::new(
            Arc::clone(&search.ask),
            workspace,
            search.wake.clone(),
            search.models.clone(),
        )));
    }
    // What the person types, handed to their agent: the one send a window's box and the canvas
    // run. A host that sends nothing (the page) offers no such row.
    #[cfg(target_os = "linux")]
    if !matches!(scope, Scope::InApp(_))
        && let Some(agents) = search.agents.borrow().as_ref()
    {
        let kept = agents
            .iter()
            .find(|agent| agent.workspace == workspace)
            .map(|agent| (agent.id.clone(), agent.name.clone()));
        sources.push(Held::new(crate::search_agent::AskAgent::new(
            workspace,
            crate::agents::agent_recipient(kept),
        )));
    }
    // The apps of the reading the sources were just built from, so one search never mixes two.
    let applications = SessionSearch::installed_applications(search.kept.borrow().reading.as_ref());
    // What fills a resource-kind parameter's field, where this session serves the workspace's
    // catalogue: the kind's list action is asked through the same action host.
    let resource_options = search.catalogue.borrow().clone().map(|catalogue| {
        let silent: std::sync::Arc<dyn Fn() + Send + Sync> = std::sync::Arc::new(|| {});
        let wake = search.wake.clone().unwrap_or(silent);
        let caller = tilo_actions::remote::Caller::local_person(search.person.as_str())
            .in_workspace(workspace);
        std::sync::Arc::new(tilo_desktop_search::providers::ResourceLists::new(
            catalogue, caller, wake,
        )) as std::sync::Arc<dyn tilo_desktop_search::providers::ResourceSource>
    });
    SearchHostFeed {
        applications,
        published,
        sources,
        resource_options,
        preferences: search.preferences.borrow().clone(),
        now: ui::time::unix_now(),
        usage: search.usage.borrow().clone(),
    }
}

/// The desktop's one search as a host that holds it offers it
/// ([`ResourceSearch`]): the same search the palette and text typed on the
/// canvas run ([`SessionSearch::find_soon`]), so an agent's `find` answers what
/// those do, in their order — asked on the loop's pass and answered when its
/// sources answer, never waited for there.
impl ResourceSearch for SearchThrough {
    fn find(
        &self,
        caller: &Caller,
        needle: &str,
    ) -> tilo_actions::remote::Pending<Result<Vec<Found>, ActionCallError>> {
        // An agent finds what the workspace it works in holds: the search is the person's, over
        // every workspace they see, and an agent is in one.
        let workspace = if caller.acting().is_person() {
            None
        } else {
            match caller.workspace() {
                Some(workspace) => Some(workspace.to_owned()),
                None => {
                    return tilo_actions::remote::Pending::ready(Err(
                        ActionCallError::Unavailable {
                            reason:
                                "an agent searches the workspace it works in, and works in none"
                                    .to_owned(),
                        },
                    ));
                }
            }
        };
        let (done, answer) = tilo_actions::remote::pending();
        self.ask(needle.trim().to_owned(), workspace, done);
        answer.map(Ok)
    }
}

/// A host's [`ResourceSearch`] that asks the thread holding the desktop, as the session's own APIs
/// are asked: the session's search is not `Send` , and every call of it runs on the loop's pass.
pub(crate) struct SearchThrough(
    std::sync::Arc<dyn Fn(String, Option<String>, FoundSearch) + Send + Sync>,
);

/// Where the session's answer to a search goes.
pub(crate) type FoundSearch = tilo_actions::remote::Completer<Vec<Found>>;

impl SearchThrough {
    pub fn new(
        ask: std::sync::Arc<dyn Fn(String, Option<String>, FoundSearch) + Send + Sync>,
    ) -> Self {
        Self(ask)
    }

    /// Ask for what `needle` finds, in `workspace` alone where one is named
    /// ([`SessionSearch::find_soon`]).
    fn ask(&self, needle: String, workspace: Option<String>, done: FoundSearch) {
        (self.0)(needle, workspace, done);
    }
}

/// The search hits `found` names, as a caller who addresses resources reads them: the resource each
/// is about, best first, and never below the one `place_found` places. The canvas stands the hit
/// [`layout::placed`] chooses; `find` leads with that same hit, so an agent asked for a text
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
    let lead = tilo_session_core::layout::placed(&hits, text, shows);
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
        // Two rows naming one resource with the arguments that open it are one
        // result: a page of Settings and another page are distinct resources,
        // and two items a record holds are distinct anchors.
        if !out
            .iter()
            .any(|seen| seen.resource == found.resource && seen.arguments == found.arguments)
        {
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
        // A row runs the action its hit names, where the hit names it: the
        // resource's own action, or the `open` of the resource holding an item,
        // told the item as its anchor.
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
        | Target::Content { .. }
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
        provider: None,
        when: None,
        ..Found::default()
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    use tilo_desktop_search::providers::AppEntry;
    use tilo_desktop_search::providers::host_apps::{HOST_APPS_SOURCE, HostAppEntry};

    use super::*;

    /// The action host a test session reaches: it notes what search opens — a URI in which
    /// workspace, an app's resource by the provider and action it was invoked at — and carries out
    /// nothing else.
    struct Opening(Rc<std::cell::RefCell<Vec<(String, String)>>>);

    impl AppActions for Opening {
        fn invoke(
            &self,
            target: RemoteActionTarget,
            _: serde_json::Map<String, serde_json::Value>,
        ) -> Result<(), ApiError> {
            self.0
                .borrow_mut()
                .push((target.provider, target.address.action));
            Ok(())
        }

        fn open_uri(&self, workspace: &str, uri: &str) -> Result<(), ApiError> {
            self.0
                .borrow_mut()
                .push((workspace.to_owned(), uri.to_owned()));
            Ok(())
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
                    })
                    .collect(),
                named: Vec::new(),
                host_apps: text
                    .lines()
                    .map(|name| HostAppEntry {
                        desktop_id: format!("org.example.{name}.desktop"),
                        name: format!("{name} Viewer"),
                        ..HostAppEntry::default()
                    })
                    .collect(),
                catalogue: Vec::new(),
                icons: std::collections::BTreeMap::new(),
                candidates: Vec::new(),
                declared: std::collections::BTreeMap::new(),
                resource_kinds: std::collections::BTreeMap::new(),
                launchable: Vec::new(),
                files_open: std::collections::BTreeSet::new(),
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

    tilo_test_env::scenario! {
        fn search_reads_the_lists_it_was_given;

        phase a_change_to_the_lists_reaches_the_next_search;
        {
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

        phase an_agents_search_finds_nothing_of_the_shells_own_rows;
        // The shell's own rows (a window, by the title of what it shows) span every workspace: the
        // person's search finds them, and an agent's, confined to one workspace, never does.
        {
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
                "an agent in one workspace finds no window of the shell: {agent:?}"
            );
        }

        phase widening_search_lists_other_workspaces_places_by_name;
        {
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

        phase fixed_lists_keep_what_search_built_from_them;
        {
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

    /// A row is collected from one reading of the device's indexes and chosen later, so the record
    /// it names may have been dropped from every index in between: it is refused rather than named
    /// in a workspace as if it were still there.
    #[test]
    fn a_row_for_a_record_no_workspace_names_any_more_is_refused() {
        const ELSEWHERE: &str = "6d1c1f36-0000-4000-8000-000000000011";
        const HERE: &str = "6d1c1f36-0000-4000-8000-000000000012";
        const STALE: &str = "file:/work/Stale plan.odt";
        const FRESH: &str = "file:/work/Fresh plan.odt";
        let device = crate::records::testing::Device::holding(&[ELSEWHERE, HERE]);
        device.name("so.tilo.notes", ELSEWHERE, &[(STALE, "document", None)]);
        let search = SessionSearch::new(
            SearchOptions {
                records: Some(device.search()),
                ..SearchOptions::default()
            },
            None,
            "",
            None,
            None,
            None,
        );
        // The row is collected now and chosen below.
        let row = Target::Invoke(Box::new(
            InvokeTarget::action(
                DESKTOP_APP,
                ELSEWHERE,
                record_rows::RECORDS_RESOURCE,
                record_rows::BRING_HERE,
            )
            .with_argument(record_rows::RECORD_PARAM, STALE)
            .with_argument(record_rows::KIND_PARAM, "document")
            .with_argument(record_rows::PACKAGE_PARAM, "so.tilo.notes"),
        ));
        let held = |record: &str| {
            search
                .records
                .as_ref()
                .is_some_and(|records| records.holds(record))
        };
        device.until(|| held(STALE));
        // The workspace holding it stops naming it, in one write that names another record.
        device.name("so.tilo.notes", ELSEWHERE, &[(FRESH, "document", None)]);
        device.until(|| held(FRESH));
        let refused = search.perform(HERE, row, &mut |_| Ok(()));
        assert!(
            matches!(
                refused,
                Err(ApiError::Unavailable { ref code, .. }) if code == "record_not_held"
            ),
            "a record no workspace names is refused: {refused:?}"
        );
        assert!(
            device.names("so.tilo.notes", HERE).is_empty(),
            "nothing is named here"
        );
        drop(device);
    }

    tilo_test_env::scenario! {
        fn a_record_chosen_in_search_opens_in_its_app;

        phase a_record_held_here_chosen_in_search_opens_in_its_app;
        // A record this workspace holds is found by the name its index gives it, and choosing it opens
        // it in its app as a link to it does: the app's `open` on that record, through the action host,
        // which brings the window on it forward or opens the app there.
        {
            const PDF: &str = ui::apps::PDF_VIEWER;
            const FORM: &str = "uuid:6d1c1f36-0000-4000-8000-0000000000aa";
            const HERE: &str = "6d1c1f36-0000-4000-8000-000000000021";
            let device = crate::records::testing::Device::holding(&[HERE]);
            device.name(PDF, HERE, &[(FORM, "pdf.document", Some("application.pdf"))]);

            let opened = Rc::new(std::cell::RefCell::new(Vec::new()));
            let search = SessionSearch::new(
                SearchOptions {
                    records: Some(device.search()),
                    ..SearchOptions::default()
                },
                None,
                "",
                Some(Rc::new(Opening(Rc::clone(&opened)))),
                None,
                None,
            );
            device.until(|| {
                search
                    .records
                    .as_ref()
                    .is_some_and(|records| records.holds(FORM))
            });
            drop(device);
            let feed = search.feed(HERE, &Scope::Workspace);
            let hit = feed
                .sources
                .iter()
                .find(|source| source.id() == "records")
                .and_then(|source| source.query("application").into_iter().next())
                .expect("the form is found by its name");
            assert_eq!(hit.title, "application.pdf");
            search
                .perform(HERE, hit.target, &mut |_| Ok(()))
                .expect("the record opens");
            let link = InvokeTarget::place(
                HERE,
                &crate::platform::notice::Place {
                    app: PDF.into(),
                    record: FORM.into(),
                    ..Default::default()
                },
            )
            .remote();
            assert_eq!(
                opened.borrow().as_slice(),
                [(link.provider, link.address.action)],
                "the record opens in its app, as a link to it does"
            );
        }

        phase a_search_with_no_action_host_finds_a_workspace_record_by_name_and_opens_it_in_its_app;
        // The session's search mounted with no action host, whose shell opens an app itself, over
        // the records the workspace holds. A PDF its workspace's index names `application.pdf` is
        // offered for "application", and choosing it opens PDF Viewer on that record on the shell.
        {
            const PDF: &str = ui::apps::PDF_VIEWER;
            const FORM: &str = "uuid:6d1c1f36-0000-4000-8000-0000000000aa";
            let _settled = ui::motion::testing::settled();
            let mut minter = tilo_desktop_model::IdMinter::new(7);
            let tree = tilo_session_core::desktop::seed_tree(&mut minter, "Visa", Vec::new());
            let workspace = tree
                .active_workspace()
                .map(|held| held.id().to_string())
                .expect("the shell shows a workspace");

            // The workspace's index of PDF Viewer's records, as the workspace holds it.
            let device = crate::records::testing::Device::holding(&[workspace.as_str()]);
            device.name(PDF, &workspace, &[(FORM, "pdf.document", Some("application.pdf"))]);

            let search = Rc::new(SessionSearch::new(
                SearchOptions {
                    records: Some(device.search()),
                    ..SearchOptions::default()
                },
                None,
                "ada",
                None,
                None,
                None,
            ));
            let opened = Rc::new(std::cell::RefCell::new(Vec::new()));
            let handle = tilo_session_core::desktop::state::DesktopHandle::new(tree, 7);
            let mut pass = tilo_session_core::desktop::state::DesktopPass::new(handle.clone());
            let mut windows = RecordWindows(Rc::clone(&opened));
            handle.set_viewport_width(1200.0);
            handle.set_search_host(Rc::clone(&search) as Rc<dyn SearchHost>);

            handle.open_search(Scope::Workspace, Some("application".into()));
            let started = Instant::now();
            let at = loop {
                pass.step(&mut windows);
                let (_, rows) = handle.search_rows();
                if let Some(at) = rows.iter().position(|row| row.title == "application.pdf") {
                    break at;
                }
                assert!(
                    started.elapsed() < tilo_test_env::hang_bound(),
                    "the form is offered by its name: {rows:?}"
                );
                std::thread::park_timeout(Duration::from_millis(5));
            };
            handle.choose_search_row(at, None, None);
            for _ in 0..3 {
                pass.step(&mut windows);
            }
            assert!(
                search.perform_choices(&handle, &mut |_| Ok(())).is_empty(),
                "the page's shell opens the record itself"
            );
            assert_eq!(
                opened.borrow().as_slice(),
                [(PDF.to_owned(), FORM.to_owned())],
                "PDF Viewer opens on the form"
            );
        }

        phase a_file_record_chosen_in_search_is_brought_here_and_opened;
        // A record kept as a file is brought here and opened: the row a person chose in search leaves
        // the workspace naming the record *and* shows them the document, in the app that takes its
        // type.
        {
            let home = tilo_test_env::TestHome::new().expect("a home");
            let file = home.home().join("Budget.note");
            std::fs::write(&file, "what the record holds").expect("the record's file");
            let record = ui::resource::DocumentId::for_file(&file);
            // The workspace that holds it names it in its index.
            const ELSEWHERE: &str = "6d1c1f36-0000-4000-8000-000000000031";
            const HERE: &str = "6d1c1f36-0000-4000-8000-000000000032";
            let device = crate::records::testing::Device::holding(&[ELSEWHERE, HERE]);
            device.name("so.tilo.notes", ELSEWHERE, &[(record.as_str(), "document", None)]);

            let opened = Rc::new(std::cell::RefCell::new(Vec::new()));
            let search = SessionSearch::new(
                SearchOptions {
                    records: Some(device.search()),
                    ..SearchOptions::default()
                },
                None,
                "",
                Some(Rc::new(Opening(Rc::clone(&opened)))),
                None,
                None,
            );
            device.until(|| {
                search
                    .records
                    .as_ref()
                    .is_some_and(|records| records.holds(record.as_str()))
            });
            let row = Target::Invoke(Box::new(
                InvokeTarget::action(
                    DESKTOP_APP,
                    HERE,
                    record_rows::RECORDS_RESOURCE,
                    record_rows::BRING_HERE,
                )
                .with_argument(record_rows::RECORD_PARAM, record.as_str())
                .with_argument(record_rows::KIND_PARAM, "document")
                .with_argument(record_rows::PACKAGE_PARAM, "so.tilo.notes"),
            ));
            search
                .perform(HERE, row, &mut |_| Ok(()))
                .expect("the record is brought here and opened");
            assert_eq!(
                opened.borrow().as_slice(),
                [(HERE.to_owned(), tilo_desktop_search::file_uri(&file))],
                "the document is opened by its path, not by the record id's `file:/…`"
            );
            device.until(|| device.names("so.tilo.notes", HERE) == [record.as_str()]);
            drop(device);

            // The file going away leaves the record held and says what is gone.
            std::fs::remove_file(&file).expect("the file goes");
            let row = Target::Invoke(Box::new(
                InvokeTarget::action(
                    DESKTOP_APP,
                    HERE,
                    record_rows::RECORDS_RESOURCE,
                    record_rows::BRING_HERE,
                )
                .with_argument(record_rows::RECORD_PARAM, record.as_str())
                .with_argument(record_rows::KIND_PARAM, "document")
                .with_argument(record_rows::PACKAGE_PARAM, "so.tilo.notes"),
            ));
            assert!(
                matches!(
                    search.perform(HERE, row, &mut |_| Ok(())),
                    Err(ApiError::Failed(ref reason)) if reason.contains("is gone")
                ),
                "a record whose file is gone is still held, and the launch says why it does not open"
            );
        }
    }

    /// The windows a page's shell opens on records: each record launch opens its app on that
    /// record, as the page does.
    struct RecordWindows(Rc<std::cell::RefCell<Vec<(String, String)>>>);

    impl tilo_session_core::desktop::state::TileSource for RecordWindows {
        fn tile(
            &mut self,
            _: &tilo_desktop_model::Workspace,
            tab: &tilo_desktop_model::Tab,
        ) -> ui::widget::TileHost {
            ui::widget::TileHost::new(ui::widget::TileContent::Notice {
                title: tab.title.clone(),
                detail: String::new(),
            })
        }

        fn launch(
            &mut self,
            _: &tilo_desktop_model::Workspace,
            id: u64,
            app: &str,
            _beside: Option<u64>,
        ) -> Option<tilo_desktop_model::Tab> {
            let (app, record) = tilo_desktop_search::launched_record(app)?;
            self.0
                .borrow_mut()
                .push((app.to_owned(), record.to_owned()));
            Some(tilo_desktop_model::Tab {
                id,
                title: record.into(),
                tile: tilo_desktop_model::TileContent::LocalApp {
                    app_id: app.into(),
                    label: record.into(),
                    note: String::new(),
                },
                app_title: None,
                view: String::new(),
                pinned: false,
            })
        }
    }

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
            at: 1,
            device: None,
        }
    }

    /// A window's search reads what its app published into the device's index, and none of
    /// another app's: one index, narrowed to the app the window shows. The same search over
    /// everything finds it too.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_windows_search_reads_what_its_app_published_and_no_other() {
        use std::sync::Arc;

        let index = Arc::new(tilo_workspace_service::search::AppContentIndex::in_memory());
        let mut publisher = ui::search::index_publication::Publisher::default();
        for (app, key, title) in [
            ("so.tilo.chat", "chat.workspace/channels/c_voice", "voice"),
            ("so.tilo.mail", "mail.inbox", "Inbox"),
        ] {
            let entry = ui::search::index_publication::IndexEntry {
                key: key.to_owned(),
                resource: key.to_owned(),
                title: title.to_owned(),
                ..ui::search::index_publication::IndexEntry::default()
            };
            for delta in publisher.publish(
                ui::search::index_publication::IndexState {
                    generation: 1,
                    secrets_unlocked: None,
                },
                vec![entry],
            ) {
                index
                    .publish(
                        "workspace",
                        app,
                        "tab:0001",
                        &delta.encode().expect("a delta encodes"),
                    )
                    .expect("the index takes the delta");
            }
        }
        let search = SessionSearch::new(
            SearchOptions {
                index: Some(Arc::new(tilo_workspace_service::search::IndexSearch::new(
                    Arc::clone(&index),
                    Arc::new(|_, _| true),
                ))),
                ..SearchOptions::default()
            },
            None,
            "ada",
            None,
            None,
            None,
        );

        let (in_window, shell_actions) = search.sources(
            "workspace",
            &Scope::InApp(ui::search::AppScope::of("so.tilo.chat")),
        );
        assert!(
            shell_actions.is_empty(),
            "the shell's own actions are not the window's"
        );
        let titles = |needle: &str| -> Vec<String> {
            in_window
                .iter()
                .flat_map(|source| source.query(needle))
                .map(|hit| hit.title)
                .collect()
        };
        assert_eq!(titles("voice"), ["voice"], "the app's own entries");
        assert!(
            titles("inbox").is_empty(),
            "another app's entry is not the window's: {:?}",
            titles("inbox")
        );

        let (everywhere, _) = search.sources("workspace", &Scope::Workspace);
        assert!(
            everywhere
                .iter()
                .flat_map(|source| source.query("voice"))
                .any(|hit| hit.title == "voice"),
            "the one index also answers the search over everything"
        );
    }

    /// This computer's readings are a source of search: a container is found by its image and
    /// opens the app that shows it at its page, and the window scoped to that app finds it too.
    /// Turned off, the source is gone from both.
    #[test]
    fn a_reading_s_items_are_found_and_open_the_app_showing_them() {
        use tilo_desktop_search::providers::device_rows::DEVICE_ROWS_SOURCE;

        let search = SessionSearch::new(SearchOptions::default(), None, "ada", None, None, None);
        search.set_device_rows(vec![(
            "containers".to_owned(),
            tilo_actions::ReadingRow::new("c1", "build-cache")
                .subtitled("running")
                .field("postgres:17")
                .shown_by("so.tilo.containers"),
        )]);

        let source = |scope: &Scope| {
            search
                .sources("workspace", scope)
                .0
                .into_iter()
                .find(|source| source.id() == DEVICE_ROWS_SOURCE)
        };
        let found = source(&Scope::Workspace)
            .expect("the device's readings answer search")
            .query("postgres");
        assert_eq!(found.len(), 1, "a container is found by its image");
        assert_eq!(found[0].title, "build-cache");
        let Target::Invoke(target) = &found[0].target else {
            panic!(
                "a container opens the app showing it: {:?}",
                found[0].target
            );
        };
        assert_eq!(target.app, "so.tilo.containers");
        assert_eq!(
            target.arguments[tilo_desktop_search::OPEN_ANCHOR],
            "containers/c1",
            "the window opens at the container's page"
        );

        let window = source(&Scope::InApp(ui::search::AppScope::of(
            "so.tilo.containers",
        )))
        .expect("the window of the app showing it finds it");
        assert_eq!(window.query("postgres").len(), 1);
        assert!(
            source(&Scope::InApp(ui::search::AppScope::of("so.tilo.logs"))).is_none(),
            "another app's window finds nothing of this reading"
        );
    }

    /// A program this computer runs is found by its name and opens the Windows Apps window at
    /// its page, whether or not that window is open: the row the system service publishes for
    /// the reading's item.
    #[test]
    fn a_windows_program_is_found_by_its_name_and_opens_the_app_at_it() {
        let search = SessionSearch::new(SearchOptions::default(), None, "ada", None, None, None);
        search.set_device_rows(vec![(
            "windows-apps".to_owned(),
            tilo_actions::ReadingRow::new("adobe-reader", "Adobe Acrobat Reader")
                .subtitled("Adobe")
                .grouped("Windows apps")
                .field("Wine 11.0")
                .fact("Wine 11.0")
                .shown_by("so.tilo.windows"),
        )]);
        let found = |scope: &Scope| -> Vec<tilo_desktop_search::Hit> {
            let (sources, _) = search.sources("garden", scope);
            sources
                .iter()
                .flat_map(|source| source.query("acrobat"))
                .collect()
        };
        let hits = found(&Scope::Workspace);
        let hit = hits
            .iter()
            .find(|hit| hit.title == "Adobe Acrobat Reader")
            .unwrap_or_else(|| panic!("the program is found: {hits:?}"));
        assert_eq!(hit.subtitle.as_deref(), Some("Adobe"));
        let Target::Invoke(invoke) = &hit.target else {
            panic!("a program opens the app that shows it: {:?}", hit.target);
        };
        assert_eq!(invoke.app, "so.tilo.windows");
        assert_eq!(invoke.workspace, "garden");
        assert_eq!(
            invoke.arguments[tilo_desktop_search::OPEN_ANCHOR],
            "windows-apps/adobe-reader",
            "the window opens at the program's page"
        );
        let in_windows = Scope::InApp(ui::search::AppScope::of("so.tilo.windows"));
        assert_eq!(
            found(&in_windows).len(),
            1,
            "the window's own search finds it"
        );
        let in_notes = Scope::InApp(ui::search::AppScope::of("so.tilo.notes"));
        assert!(
            found(&in_notes).is_empty(),
            "another window's search is that window's alone"
        );
    }

    /// Who can be found in a workspace is one list: a member, named by their presence where the
    /// governance has no name for them, is found by desktop search, which opens the conversation
    /// with them, and addressed by a box; an agent both present and listed by the workspace is one
    /// row; the person searching is neither.
    #[test]
    fn people_and_agents_are_found_once_from_one_list() {
        use tilo_desktop_search::providers::happening::PersonHere;
        use ui::floating_surface::RecipientKind::{Agent, Person};

        let (ada, ben, ws) = ("ad".repeat(32), "be".repeat(32), "garden".to_owned());
        let search = SessionSearch::new(SearchOptions::default(), None, &ada, None, None, None);
        let member = |root: &str| {
            collab_types::Member::of_person(&collab_types::Person::new(root, ""), None)
        };
        search.set_members(vec![(ws.clone(), vec![member(&ada), member(&ben)])]);
        let (id, name) = ("helper".to_owned(), "Helper".to_owned());
        let workspace = ws.clone();
        search.set_listed_agents(&[ListedAgent {
            avatar: None,
            workspace,
            id: id.clone(),
            name: name.clone(),
            retired: false,
        }]);
        let here = |id: &str, name: &str, agent| PersonHere {
            id: id.to_owned(),
            name: name.to_owned(),
            colour: (0, 0, 0),
            agent,
            away: false,
            tab: None,
        };
        search.set_here(vec![WorkspaceHere {
            workspace: ws.clone(),
            name: "Garden".to_owned(),
            active: true,
            people: vec![here(&ben, "Ben", false), here(&id, &name, true)],
            windows: Vec::new(),
            calls: Vec::new(),
        }]);

        let found: Vec<_> = search
            .recipients(&ws)
            .into_iter()
            .map(|r| (r.kind, r.id, r.name))
            .collect();
        assert_eq!(
            found,
            [(Person, ben.clone(), "Ben".to_owned()), (Agent, id, name)]
        );
        let people = |needle: &str| -> Vec<tilo_desktop_search::Hit> {
            let hits = search.find_in(needle, Some(&ws));
            hits.into_iter()
                .filter(|hit| hit.provider == "people")
                .collect()
        };
        let ben_rows = people("Ben");
        assert_eq!(
            ben_rows.iter().map(|hit| &hit.id).collect::<Vec<_>>(),
            [&ben]
        );
        let Target::Invoke(open) = &ben_rows[0].target else {
            panic!("a person's row opens something: {:?}", ben_rows[0].target);
        };
        let opened = (open.app.as_str(), open.address.action.as_str());
        assert_eq!(opened, ("so.tilo.chat", "open-direct-conversation"));
        assert!(
            people("Someone without a name yet").is_empty(),
            "nobody is offered to themselves"
        );
    }

    /// A retired agent takes no new work, so no box offers it; what it said before it retired is
    /// still named by the workspace's list, never by its id.
    #[test]
    fn a_retired_agent_is_named_but_not_offered() {
        use ui::floating_surface::RecipientKind::Agent;
        let ws = "garden".to_owned();
        let search = SessionSearch::new(
            SearchOptions::default(),
            None,
            &"ad".repeat(32),
            None,
            None,
            None,
        );
        search.set_listed_agents(&[
            ListedAgent {
                avatar: None,
                workspace: ws.clone(),
                id: "video-agent".to_owned(),
                name: "Video agent".to_owned(),
                retired: false,
            },
            ListedAgent {
                avatar: None,
                workspace: ws.clone(),
                id: "old-video".to_owned(),
                name: "Old video".to_owned(),
                retired: true,
            },
        ]);
        assert_eq!(
            search.agent_name("old-video").as_deref(),
            Some("Old video"),
            "a message it wrote is still under its name"
        );
        let offered: Vec<_> = search
            .recipients(&ws)
            .into_iter()
            .filter(|recipient| recipient.kind == Agent)
            .map(|recipient| recipient.id)
            .collect();
        assert_eq!(
            offered,
            ["video-agent"],
            "only the working agent takes work"
        );
    }
}

/// An agent a workspace's list names, as a box that sends a message addresses it and as the
/// surfaces name it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ListedAgent {
    pub workspace: String,
    pub id: String,
    pub name: String,
    /// The picture it wears wherever an author is shown, as a `data:` URL; none shows its
    /// initials.
    pub avatar: Option<String>,
    /// It was retired: it takes no new work, and what it said before stays named by it.
    pub retired: bool,
}
