//! The programs the person downloaded, and the entry Tilo keeps for each:
//! the app they run as, and the offer to add one that has no entry yet.
//!
//! A downloaded `AppImage`, tarball program or loose binary names itself to
//! no application directory, so until something writes an entry for it no
//! menu or search lists it. When one of its windows names itself, the
//! compositor's side of this hands the window over ([`Handle::opened`]) and
//! everything after that runs on this service's worker, never on the
//! compositor's loop: reading `/proc` for the process, deciding whether its
//! file is a download ([`desktop_entry::downloaded::classify`]), reading an
//! `AppImage`'s image or the program's folder for its name and artwork, and
//! writing the entry.
//!
//! What it does with what it found:
//! - an app that already has an entry, now run from another file (a new
//!   version, a second download), gets its entry pointed at that file, so it
//!   never has two entries;
//! - an app with no entry becomes an [`Offer`]: search shows it and the app
//!   sheet asks once whether to add it. Nothing here adds an app by itself,
//!   and a file the person said no to is not asked about again;
//! - an entry whose file went missing reads as such (`present: false`, and
//!   the app offers [`DownloadedAppAction::Locate`]), and follows the file
//!   where Tilo's filesystem moved it, else starts another copy still there.
//!
//! The entries are ordinary desktop entries ([`Store`]), so everything that
//! reads the machine's applications — search, the launcher, an app's open
//! dialog — sees them like any other host application, and starts them the
//! same `host:` way.

mod effects;

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use desktop_entry::DesktopEntry;
use desktop_entry::downloaded::{
    self, DownloadedProgram, Identity, ProcessFacts, Program, ProgramKind, Store, WindowFacts,
};
use serde_json::{Map, Value};
use tilo_actions::remote::downloaded_apps as wire;
use tilo_actions::{ActionAddress, DoorError, ResourceSpec};
use tilo_lock::MutexRecover as _;
use tilo_resource::Effect as _;
use tilo_service::{Jobs, Service, Started, Wake, Worker, decode, offer, reading};

pub use effects::{DownloadedAppAction, DownloadedAppsAction};

/// The resource every app and offer of the person's is read from.
pub const RESOURCE: &str = wire::RESOURCE;
/// The kind of [`RESOURCE`].
pub const KIND: &str = "so.tilo.downloaded-apps";
/// The kind of one app's resource, [`wire::app`].
pub const APP_KIND: &str = "so.tilo.downloaded-app";
/// What the registry and its apps are called wherever a person reads them.
pub const LABEL: &str = "Downloaded apps";

/// Where a program an `AppImage` is moved into goes: a folder in the
/// person's home they can see in their files.
const APPLICATIONS: &str = "Applications";

/// A window that named itself, as the compositor tells the session about it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObservedWindow {
    /// The compositor's id for the window.
    pub id: u64,
    /// The Wayland app id (an X11 program's class).
    pub app_id: String,
    /// The process that opened it, when the compositor knows it.
    pub pid: Option<u32>,
}

impl ObservedWindow {
    /// Whether the window names its program at all: one that has not said
    /// (`anon-…`) names nothing to look up.
    #[must_use]
    fn names_itself(&self) -> bool {
        let app_id = self.app_id.trim();
        !app_id.is_empty() && !app_id.starts_with("anon-")
    }
}

/// A program running now that has no entry: what adding it would add.
#[derive(Clone, Debug)]
struct Offer {
    program: Program,
    identity: Identity,
    /// The artwork its entry would name, kept beside the person's apps until
    /// they answer.
    artwork: Option<String>,
    /// Whether the person already said not to add this file.
    declined: bool,
    /// The windows of the program open now: the offer goes when the last
    /// one does.
    windows: BTreeSet<u64>,
}

/// What the registry knows, behind one lock: cheap to read, so a reading
/// never waits on a disk call.
#[derive(Default)]
struct State {
    /// The person's downloaded apps, as the last scan read them.
    apps: Vec<wire::DownloadedApp>,
    /// The programs running now with no entry, by the file that starts
    /// them.
    offers: BTreeMap<PathBuf, Offer>,
    /// The windows it heard about, and the process each runs.
    windows: HashMap<u64, u32>,
    /// How many windows each process has open: a program opening ten
    /// windows is one program to look at.
    open: HashMap<u32, usize>,
    /// The processes it already looked at.
    looked: HashSet<u32>,
    /// The scan generation [`Self::apps`] was read at.
    scan: Option<u64>,
    /// The entries whose file was missing at the last scan: only one that
    /// has just gone missing is followed.
    missing: HashSet<String>,
}

impl State {
    /// The reading, as the resource serves it.
    fn reading(&self) -> wire::DownloadedApps {
        wire::DownloadedApps {
            apps: self.apps.clone(),
            offers: self
                .offers
                .values()
                .map(|offer| wire::DownloadedOffer {
                    file: offer.program.path.display().to_string(),
                    name: offer.identity.name.clone(),
                    kind: kind(offer.program.kind),
                    artwork: offer.artwork.clone(),
                    declined: offer.declined,
                })
                .collect(),
        }
    }
}

/// What every job runs over: the store the entries live in, the person's
/// home, and what the last look found.
struct Core {
    /// `None` where there is no data directory to keep apps in: nothing is
    /// offered or added then.
    store: Option<Store>,
    home: Option<PathBuf>,
    state: Mutex<State>,
}

/// Where the registry is driven from: the service, and the windows the
/// compositor hands over from its own loop.
#[derive(Clone)]
pub struct Handle {
    core: Arc<Core>,
    jobs: Jobs<Core, bool>,
}

impl fmt::Debug for Handle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("Handle").finish_non_exhaustive()
    }
}

impl Handle {
    /// A window named itself: look at the process behind it, on the worker.
    pub fn opened(&self, window: ObservedWindow) {
        if !window.names_itself() {
            return;
        }
        self.jobs.run(move |core| core.opened(&window, None));
    }

    /// A window named itself and its process is `facts`, already read: what
    /// a test drives, and what a caller that had to read `/proc` anyway
    /// passes on.
    pub fn observed(&self, window: ObservedWindow, facts: ProcessFacts) {
        if !window.names_itself() {
            return;
        }
        self.jobs.run(move |core| core.opened(&window, Some(facts)));
    }

    /// The window is gone: an offer waits for the program's last one.
    pub fn closed(&self, id: u64) {
        self.jobs.run(move |core| core.closed(id));
    }

    /// Read the entries again, on the worker: an entry this session wrote,
    /// or the machine's applications changing under it.
    pub fn refresh(&self) {
        self.core
            .mark(desktop_entry::cache::cached_scan_generation());
        self.jobs.run(Core::rebuild);
    }

    /// Add the program at `file` to the person's apps, moving an `AppImage`
    /// into their Applications folder first when they asked for that.
    pub fn add(&self, file: PathBuf, move_to_applications: bool) {
        self.jobs
            .run(move |core| core.add(&file, move_to_applications));
    }

    /// Do not add the program at `file`, and do not ask about it again.
    pub fn decline(&self, file: PathBuf) {
        self.jobs.run(move |core| core.decline(&file));
    }

    /// Point the entry `desktop_id` at `path`, the file the person found.
    pub fn locate(&self, desktop_id: String, path: PathBuf) {
        self.jobs.run(move |core| core.locate(&desktop_id, &path));
    }

    /// Take the entry `desktop_id` off the person's apps.
    pub fn remove(&self, desktop_id: String) {
        self.jobs.run(move |core| core.remove(&desktop_id));
    }

    /// Move every copy of the app `desktop_id` to the trash, then take the
    /// entry off.
    pub fn uninstall(&self, desktop_id: String) {
        self.jobs.run(move |core| core.uninstall(&desktop_id));
    }

    /// The apps the person has, and the programs running with no entry yet.
    #[must_use]
    pub fn reading(&self) -> wire::DownloadedApps {
        self.core.state.lock_recover().reading()
    }
}

/// The programs the person downloaded, published as one resource the search
/// and the app sheets read.
///
/// Dropping it stops the worker every look and write ran on.
pub struct Downloaded {
    handle: Handle,
    worker: Worker<Core, bool>,
}

impl fmt::Debug for Downloaded {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("Downloaded").finish_non_exhaustive()
    }
}

impl Downloaded {
    /// The registry over the apps this session's person has. `wake` is
    /// called when a look or a write lands, so the frame that shows it is
    /// drawn without waiting for the loop's next pass.
    ///
    /// # Errors
    /// The worker did not start.
    pub fn start(wake: Option<Wake>) -> io::Result<Self> {
        let core = Arc::new(Core {
            store: Store::for_session(),
            home: tilo_paths::home(),
            state: Mutex::new(State::default()),
        });
        // The apps the person already has, and the entries whose file went
        // missing while no session ran: read once here, so the first frame
        // shows them.
        core.rebuild();
        let worker = Worker::spawn("tilo-downloaded-apps", Arc::clone(&core), wake)?;
        let handle = Handle {
            core,
            jobs: worker.jobs(),
        };
        Ok(Self { handle, worker })
    }

    /// Where to follow windows and queue the person's answers.
    #[must_use]
    pub fn handle(&self) -> Handle {
        self.handle.clone()
    }
}

impl Core {
    /// Record `window`, and look at its program unless this process was
    /// looked at already. Returns whether anything a reader sees changed.
    fn opened(&self, window: &ObservedWindow, facts: Option<ProcessFacts>) -> bool {
        let Some(pid) = window.pid else {
            return false;
        };
        let look = {
            let mut state = self.state.lock_recover();
            if let Some(previous) = state.windows.insert(window.id, pid)
                && previous != pid
                && let Some(open) = state.open.get_mut(&previous)
            {
                *open = open.saturating_sub(1);
            }
            let open = state.open.entry(pid).or_default();
            *open = open.saturating_add(1);
            state.looked.insert(pid)
        };
        if !look {
            return false;
        }
        let Some(facts) = facts.or_else(|| downloaded::process_facts(pid)) else {
            return false;
        };
        self.look(window, &facts)
    }

    /// What the program behind `window` is, and what to do about it.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the offer and the window it came from are recorded in one hold, so the same program opened twice makes one offer"
    )]
    fn look(&self, window: &ObservedWindow, facts: &ProcessFacts) -> bool {
        let (Some(store), Some(home)) = (&self.store, &self.home) else {
            return false;
        };
        let Some(program) = downloaded::classify(facts, home) else {
            return false;
        };
        // An app of the machine's own that names this window — an installed
        // one, a hand-written entry — is already in the person's list.
        if installed(&window.app_id) {
            return false;
        }
        // A copy only its window names joins the entry another copy of the
        // same program has.
        let identity = store.identify(
            &program,
            &WindowFacts {
                app_id: window.app_id.clone(),
                icon: None,
            },
        );
        match store.update_location(&identity, &program) {
            Ok(Some(desktop_id)) => {
                tracing::info!(
                    %desktop_id,
                    program = %program.path.display(),
                    "a downloaded app now runs from another file"
                );
                desktop_entry::cache::refresh();
                return self.rebuild();
            }
            Ok(None) if store.lookup(&identity.key).is_some() => return false,
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(%error, "could not point a downloaded app's entry at its file");
                return false;
            }
        }
        // The artwork its entry would have, kept where the entry will name
        // it; a file the person said no to is offered all the same, marked.
        let artwork = store
            .keep_icon(&identity)
            .ok()
            .flatten()
            .map(|path| path.display().to_string());
        let declined = store.declined(&program.path);
        let mut state = self.state.lock_recover();
        let fresh = !state.offers.contains_key(&program.path);
        let offer = state
            .offers
            .entry(program.path.clone())
            .or_insert_with(|| Offer {
                program,
                identity,
                artwork,
                declined,
                windows: BTreeSet::new(),
            });
        offer.windows.insert(window.id);
        fresh
    }

    /// The window is gone, and the offer with it once the program's last one
    /// is.
    fn closed(&self, id: u64) -> bool {
        let mut state = self.state.lock_recover();
        let Some(pid) = state.windows.remove(&id) else {
            return false;
        };
        if let Some(open) = state.open.get_mut(&pid) {
            *open = open.saturating_sub(1);
            if *open == 0 {
                state.open.remove(&pid);
                state.looked.remove(&pid);
            }
        }
        let mut changed = false;
        for offer in state.offers.values_mut() {
            if offer.windows.remove(&id) {
                changed = true;
            }
        }
        if changed {
            let before = state.offers.len();
            state.offers.retain(|_, offer| !offer.windows.is_empty());
            changed = state.offers.len() != before;
        }
        changed
    }

    /// Read the entries again: the apps the person has, and the files that
    /// went missing since the last scan.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the read apps and the missing ones are saved in one hold, and the files are followed after it"
    )]
    fn rebuild(&self) -> bool {
        let (generation, scan) = desktop_entry::cache::cached_scan_snapshot();
        let apps: Vec<wire::DownloadedApp> = scan.entries.iter().filter_map(app).collect();
        let missing: HashSet<String> = scan
            .entries
            .iter()
            .filter(|entry| {
                entry
                    .downloaded
                    .as_ref()
                    .is_some_and(|downloaded| !downloaded.present)
            })
            .map(|entry| entry.id.clone())
            .collect();
        let (changed, follow) = {
            let mut state = self.state.lock_recover();
            let changed = state.apps != apps;
            state.apps = apps;
            state.scan = Some(generation);
            let follow: Vec<(String, Option<[u8; 16]>)> = missing
                .iter()
                .filter(|desktop_id| !state.missing.contains(*desktop_id))
                .filter_map(|desktop_id| {
                    let downloaded = scan.find(desktop_id)?.downloaded.as_ref()?;
                    (downloaded.record().is_some() || !downloaded.locations.is_empty())
                        .then(|| (desktop_id.clone(), downloaded.record()))
                })
                .collect();
            state.missing = missing;
            (changed, follow)
        };
        for (desktop_id, record) in follow {
            self.follow(&desktop_id, record);
        }
        changed
    }

    /// The entry `desktop_id`'s file is gone: point it at the file where
    /// Tilo's filesystem moved it, else start another copy still there.
    fn follow(&self, desktop_id: &str, record: Option<[u8; 16]>) -> bool {
        let Some(store) = &self.store else {
            return false;
        };
        let moved = record
            .and_then(workspace_record)
            .filter(|path| std::fs::metadata(path).is_ok_and(|meta| meta.is_file()));
        let changed = match moved {
            Some(path) => store.locate(desktop_id, &path).is_ok(),
            None => match store.fall_back(desktop_id) {
                Ok(copy) => copy.is_some(),
                Err(error) => {
                    tracing::warn!(%desktop_id, %error, "could not follow a downloaded app's file");
                    false
                }
            },
        };
        if changed {
            desktop_entry::cache::refresh();
        }
        changed
    }

    /// Add the program `file` to the person's apps.
    fn add(&self, file: &Path, move_to_applications: bool) -> bool {
        let (Some(store), Some(home)) = (&self.store, &self.home) else {
            return false;
        };
        let offer = {
            let state = self.state.lock_recover();
            state
                .offers
                .get(file)
                .map(|offer| (offer.program.clone(), offer.identity.clone()))
        };
        let Some((mut program, identity)) = offer else {
            tracing::warn!(file = %file.display(), "asked to add a downloaded program that is not running");
            return false;
        };
        // Only an `AppImage` is one self-contained file; anything else may
        // need the files beside it and stays where it is.
        if move_to_applications && program.kind == ProgramKind::AppImage {
            let folder = home.join(APPLICATIONS);
            match downloaded::move_into(&program.path, &folder) {
                Ok(moved) => program.path = moved,
                Err(error) => {
                    tracing::warn!(%error, program = %program.path.display(), "could not move a downloaded app into Applications");
                    return false;
                }
            }
        }
        match store.add(&identity, &program) {
            Ok(desktop_id) => tracing::info!(
                %desktop_id,
                program = %program.path.display(),
                "added a downloaded app"
            ),
            Err(error) => {
                tracing::warn!(%error, program = %program.path.display(), "could not add a downloaded app");
                return false;
            }
        }
        self.state.lock_recover().offers.remove(file);
        desktop_entry::cache::refresh();
        self.rebuild()
    }

    /// Remember that the person said no to the program `file`. The offer
    /// stays, so search can add it later; nothing asks again.
    fn decline(&self, file: &Path) -> bool {
        let Some(store) = &self.store else {
            return false;
        };
        let some_offer = {
            let state = self.state.lock_recover();
            state.offers.get(file).map(|offer| offer.identity.clone())
        };
        let Some(identity) = some_offer else {
            return false;
        };
        if let Err(error) = store.decline(file) {
            tracing::warn!(%error, file = %file.display(), "could not remember a declined downloaded app");
        }
        if let Err(error) = store.discard_icon(&identity) {
            tracing::warn!(%error, "could not discard a declined downloaded app's artwork");
        }
        let mut state = self.state.lock_recover();
        match state.offers.get_mut(file) {
            Some(offer) if !offer.declined => {
                offer.declined = true;
                offer.artwork = None;
                true
            }
            _ => false,
        }
    }

    /// Point the entry `desktop_id` at `path`.
    fn locate(&self, desktop_id: &str, path: &Path) -> bool {
        let Some(store) = &self.store else {
            return false;
        };
        if let Err(error) = store.locate(desktop_id, path) {
            tracing::warn!(%desktop_id, %error, "could not point a downloaded app's entry at its file");
            return false;
        }
        desktop_entry::cache::refresh();
        self.rebuild()
    }

    /// Take the entry `desktop_id` off the person's apps; its file stays.
    fn remove(&self, desktop_id: &str) -> bool {
        let Some(store) = &self.store else {
            return false;
        };
        if let Err(error) = store.remove(desktop_id) {
            tracing::warn!(%desktop_id, %error, "could not take a downloaded app off the person's apps");
            return false;
        }
        desktop_entry::cache::refresh();
        self.rebuild()
    }

    /// Move every copy of the app `desktop_id` to the trash, then take the
    /// entry off.
    fn uninstall(&self, desktop_id: &str) -> bool {
        let Some(store) = &self.store else {
            return false;
        };
        let Some(recorded) = store.entry(desktop_id) else {
            tracing::warn!(%desktop_id, "asked to uninstall a downloaded app that is not there");
            return false;
        };
        // Every copy goes: a leftover one would bring the app back by
        // running it, and all of them can be restored from the trash.
        for copy in std::iter::once(recorded.program).chain(recorded.locations) {
            if std::fs::symlink_metadata(&copy).is_err() {
                continue;
            }
            if let Err(error) = host_fs::trash::trash(&copy) {
                tracing::warn!(path = %copy.display(), %error, "could not move a downloaded program to the trash");
                return false;
            }
        }
        if let Err(error) = store.remove(desktop_id) {
            tracing::warn!(%desktop_id, %error, "could not take an uninstalled app off the person's apps");
            return false;
        }
        desktop_entry::cache::refresh();
        self.rebuild()
    }

    /// Note that the apps were read at `generation`, before the worker reads
    /// them: a second call before the read lands asks for nothing again.
    fn mark(&self, generation: u64) {
        self.state.lock_recover().scan = Some(generation);
    }
}

impl Service for Downloaded {
    fn resources(&self) -> Vec<ResourceSpec> {
        let apps = self.handle.core.state.lock_recover().apps.clone();
        let apps: Vec<ResourceSpec> = apps
            .iter()
            .map(|app| {
                let mut resource =
                    ResourceSpec::new(wire::app(&app.desktop_id), APP_KIND, app.name.clone())
                        .with_presentation(presentation(app));
                // Locate is how a file that is gone is found again; an app
                // that starts is not the one to point somewhere else.
                let actions = [
                    (!app.present)
                        .then(|| DownloadedAppAction::spec(wire::LOCATE))
                        .flatten(),
                    DownloadedAppAction::spec(wire::REMOVE),
                    DownloadedAppAction::spec(wire::UNINSTALL),
                ];
                for spec in actions.into_iter().flatten() {
                    resource = resource.with_action(spec);
                }
                resource
            })
            .collect();
        vec![
            offer::<DownloadedAppsAction>(ResourceSpec::new(RESOURCE, KIND, LABEL))
                .with_children(apps),
        ]
    }

    fn read(&self, resource: &str) -> Option<Value> {
        let now = self.handle.reading();
        if resource == RESOURCE {
            return Some(reading(&now));
        }
        let desktop_id = resource.strip_prefix(wire::APP_PREFIX)?;
        let app = now
            .apps
            .into_iter()
            .find(|app| app.desktop_id == desktop_id)?;
        Some(reading(&app))
    }

    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, DoorError> {
        let published = self.resources();
        if resource == RESOURCE {
            let effect = decode::<DownloadedAppsAction>(&published, resource, action, arguments)?;
            match effect {
                DownloadedAppsAction::Add {
                    file,
                    move_to_applications,
                } => self
                    .handle
                    .add(PathBuf::from(file), move_to_applications.unwrap_or(false)),
                DownloadedAppsAction::Decline { file } => self.handle.decline(PathBuf::from(file)),
            }
            return Ok(Started::Accepted);
        }
        let Some(desktop_id) = resource.strip_prefix(wire::APP_PREFIX) else {
            return Err(DoorError::Unknown(ActionAddress::new(resource, action)));
        };
        let desktop_id = desktop_id.to_owned();
        let effect = decode::<DownloadedAppAction>(&published, resource, action, arguments)?;
        match effect {
            DownloadedAppAction::Locate { path } => {
                self.handle.locate(desktop_id, PathBuf::from(path));
            }
            DownloadedAppAction::Remove => self.handle.remove(desktop_id),
            DownloadedAppAction::Uninstall => self.handle.uninstall(desktop_id),
        }
        Ok(Started::Accepted)
    }

    fn poll(&mut self) -> bool {
        let mut changed = false;
        for landed in self.worker.landed() {
            changed |= landed;
        }
        // The scan moves on its own: an app installed, a program's file
        // deleted or put back.
        let generation = desktop_entry::cache::cached_scan_generation();
        if self
            .handle
            .core
            .state
            .lock_recover()
            .scan
            .is_none_or(|read| read != generation)
        {
            self.handle.refresh();
            changed = true;
        }
        changed
    }

    fn settle(&mut self) {
        let _ = self.worker.settle();
    }
}

/// One app, as a person reads it in a list or a result: what it is, where its
/// file is, and one line about it.
fn presentation(app: &wire::DownloadedApp) -> tilo_actions::ResourcePresentation {
    tilo_actions::ResourcePresentation {
        detail: Some(app.file.clone()),
        badge: Some(kind_label(app.kind).to_owned()),
        preview: app.comment.clone(),
    }
}

/// What a person calls a [`wire::DownloadedKind`].
#[must_use]
pub fn kind_label(kind: wire::DownloadedKind) -> &'static str {
    match kind {
        wire::DownloadedKind::AppImage => "AppImage",
        wire::DownloadedKind::Program => "Program",
    }
}

/// The wire's kind for `kind`.
fn kind(kind: ProgramKind) -> wire::DownloadedKind {
    match kind {
        ProgramKind::AppImage => wire::DownloadedKind::AppImage,
        ProgramKind::Program => wire::DownloadedKind::Program,
    }
}

/// Whether an entry of the machine's own names the window `app_id`.
fn installed(app_id: &str) -> bool {
    desktop_entry::cache::cached_scan()
        .entries
        .iter()
        .any(|entry| {
            entry.downloaded.is_none()
                && entry
                    .window_identities()
                    .iter()
                    .any(|identity| identity.eq_ignore_ascii_case(app_id))
        })
}

/// The app `entry` is, when it is one of a downloaded program.
fn app(entry: &DesktopEntry) -> Option<wire::DownloadedApp> {
    let downloaded = entry.downloaded.as_ref()?;
    Some(wire::DownloadedApp {
        desktop_id: entry.id.clone(),
        name: entry.name.clone(),
        kind: kind(downloaded.kind),
        file: downloaded.program.display().to_string(),
        copies: copies(downloaded),
        present: downloaded.present,
        artwork: entry.icon.clone(),
        comment: entry.comment.clone(),
        launch: format!("{}{}", crate::launch::HOST_PREFIX, entry.id),
    })
}

fn copies(downloaded: &DownloadedProgram) -> Vec<String> {
    downloaded
        .copies()
        .iter()
        .map(|copy| copy.display().to_string())
        .collect()
}

/// Where the file with record id `record` is now, under the workspace mounts
/// this device has: the mount's exact record index, one bounded read.
fn workspace_record(record: [u8; 16]) -> Option<PathBuf> {
    let id = uuid::Uuid::from_bytes(record);
    for root in workspace_roots() {
        let name = format!(
            "{}{id}",
            fs_protocol::projection::RECORD_INDEX_BY_ID_XATTR_PREFIX
        );
        let Some(bytes) = host_fs::attributes::get(&root, name).ok().flatten() else {
            continue;
        };
        if bytes.len() > fs_protocol::projection::MAX_RECORD_INDEX_PAGE_BYTES {
            continue;
        }
        let Ok(exact) = fs_protocol::projection::RecordIndexExactV2::from_json(&bytes) else {
            continue;
        };
        if exact.schema_version != fs_protocol::projection::RECORD_INDEX_V2_VERSION
            || !fs_protocol::projection::valid_record_index_revision(&exact.revision)
            || exact.record.record_id.0 != id
            || exact.record.is_dir
        {
            continue;
        }
        let relative = exact.record.path.strip_prefix('/')?;
        let path = Path::new(relative);
        if path
            .components()
            .any(|part| !matches!(part, std::path::Component::Normal(_)))
        {
            continue;
        }
        return Some(root.join(path));
    }
    None
}

/// The mounts Tilo's filesystem serves records on: `TILO_WORKSPACE_ROOTS`
/// where a test or another image mounts them elsewhere, else
/// `/var/workspace` when this device has it.
fn workspace_roots() -> Vec<PathBuf> {
    if let Some(value) = std::env::var_os("TILO_WORKSPACE_ROOTS") {
        return std::env::split_paths(&value)
            .filter(|path| !path.as_os_str().is_empty() && path.exists())
            .collect();
    }
    let root = PathBuf::from("/var/workspace");
    root.exists().then_some(root).into_iter().collect()
}
