//! One app instance in a tilolang host: the app's runtime over its workspace record, on the broker
//! connection the session opened for that app's launch, driving its window with component
//! patches.
//!
//! What the window's own process did before (the framework's root) is the instance's here:
//!
//! - **The record:** a [`InstanceRecord`] holds it with its undo log and save state, on the instance's
//!   connection. Each change the runtime plans is committed to it as a step of the person at the
//!   window and sent on the route; changes from the workspace come back into it.
//! - **Actions:** the instance publishes the app's resources and actions on its connection, as
//!   the toolkit publishes an app's ([`tilo_actions::door::survey`], deltas through
//!   [`SurfacePublication`]), and runs what the broker delivers: an action the broker admitted for
//!   its invoker is built through the published spec and applied as that invoker's step. It
//!   offers an action directory on the connection too, answering it from what it published
//!   ([`PublishedDirectory`]): what makes the instance one the door and agents can browse.
//! - **What every app publishes:** on its app's resource the instance publishes the history
//!   verbs (Undo, Redo, History, ...: `collaboration::history_source::verbs`), `read`
//!   (`tilo_resource::read`) and "Show this window" (`tilo_actions::remote::activate_spec`), and
//!   answers them as a framework app does: history on its record, `read` through the runtime,
//!   and "Show this window" by asking its window to come forward. The person's Undo on a notice
//!   in the window is the same `undo`, as theirs: there is one undo.
//! - **The window:** the instance sends it patches and hears what the person did over the patch
//!   connection.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

use collaboration::action_directory::PublishedDirectory;
use collaboration::history_source::verbs::{self, EDIT_TYPE, HistoryEdit, VIEW_TYPE};
use collaboration::{
    CollabEvent, CollabRequest, DocumentMutation, SurfacePublication, WorkspaceCommit,
};
use tilo_actions::{
    ActionAddress, ActionContext, ActionDescriptor, ActionInvoker, ActionRegistry, ActorKind,
    EncodedAction, InvocationId, InvocationUpdate, ResourceSpec,
};
use tilo_render_protocol::patch::{ActionCatalogue, ToUi, Transport};
use tilolang_tilo::action::{ACTION_TYPE, DynAction};
use tilolang_tilo::background::Waker;
use tilolang_tilo::bundle::Bundle;
use tilolang_tilo::host::Host;
use tilolang_tilo::instance::record::{InstanceRecord, ReadRecord, RecordError};
use tilolang_tilo::instance::{Config, Instance, Mode, Platform, Publication};
use tilolang_tilo::meta::MetaExt as _;
use tilolang_tilo::publish::HOST_RESOURCE;
use tilolang_tilo::publish::package_resource_id;
use tilolang_tilo::publish::{
    Caller, Committer, Moved, Planner, Publications, host_resource, publish_package, reached,
};
use tilolang_tilo::used::UsedApp;
use tilolang_tilo::{HostEffect, OpenEffect};

use crate::phrases::Phrases;

thread_local! {
    /// The caller of the invocation being built on this thread, as the broker delivered it.
    static CALLER: RefCell<Option<Caller>> = const { RefCell::new(None) };
}

/// Bring `host` up to date with the record and the chunks of it this replica holds: the store reads
/// and writes the resources each chunk holds where the record's manifest places them, and reads
/// the chunks it was not sent if something reads or writes in them.
fn sync_record(host: &Host, record: &InstanceRecord) {
    host.sync_chunks(record.chunks());
    host.sync_from(record.doc());
}

/// The person's date and time now: the device's clock, which is the one their window reads, so a
/// day an instance writes and a day their window heads a row by are the same day.
fn local_now() -> chrono::DateTime<chrono::Local> {
    chrono::DateTime::<chrono::Local>::from(tilo_clock::to_std(tilo_clock::wall()))
}

/// Who invokes, as the runtime reads the invoker the broker admitted.
#[must_use]
fn caller_of(invoker: &ActionInvoker) -> Caller {
    Caller {
        actor: invoker.actor.clone(),
        agent: invoker.kind != ActorKind::Person,
        on_behalf_of: invoker.on_behalf_of.clone(),
    }
}

/// What one instance is: the app, the workspace, the window and who is at it, as the session named
/// them for the app's launch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Spec {
    pub app: String,
    pub workspace: String,
    /// The window's instance.
    pub instance: u64,
    /// The participant at the window.
    pub person: String,
    /// The tilolang apps installed here, by id, each with the id its records are kept under: a
    /// package the app uses that is one of them is another app, read from its own record.
    pub installed: BTreeMap<String, String>,
    /// The person's language (`de-DE`), the one their windows read in, as the session resolved it
    /// (`InstanceRequest::language`): the window speaks it where the package ships it, and the
    /// package's source language otherwise. Empty for a session that names none, whose host reads
    /// its own process's language.
    pub language: String,
}

impl Spec {
    /// The instance `instance` of `app`, of the workspace `workspace`, with `person` at its
    /// window: no other app is installed for it to read, so what it uses is its own.
    #[must_use]
    pub fn new(
        app: impl Into<String>,
        workspace: impl Into<String>,
        instance: u64,
        person: impl Into<String>,
    ) -> Self {
        Self {
            app: app.into(),
            workspace: workspace.into(),
            instance,
            person: person.into(),
            installed: BTreeMap::new(),
            language: String::new(),
        }
    }

    /// [`Self::new`] for the window `person` reads at, in the language `language` names.
    #[must_use]
    pub fn in_language(mut self, language: impl Into<String>) -> Self {
        self.language = language.into();
        self
    }
}

/// The words the window of `spec` reads in: the language the session named for the person, or,
/// where it named none, the one this host's process runs in, against the languages `bundle`'s
/// package ships (`tilo_history::window_language`). Every word the window paints — its app's own
/// and the frames' — comes from a catalogue in that language.
fn window_phrases(base: &Phrases, spec: &Spec, bundle: &Bundle) -> Phrases {
    let catalogues = bundle.catalogues();
    let shipped: Vec<&str> = catalogues
        .iter()
        .map(|(locale, _)| locale.as_str())
        .collect();
    let language = (!spec.language.is_empty()).then_some(spec.language.as_str());
    base.for_window(language, &shipped)
}

/// What a [`Surface`]'s resources are built from: the published records' revision, the
/// loaded packages, the updates that saw a change, and the resources the window shows (its
/// rows, as published ids) — a change in what a person can reach publishes too.
type SurfaceInputs = (
    u64,
    Vec<tilolang_tilo::runtime::PackageId>,
    u64,
    Vec<String>,
);

/// The part of [`SurfaceInputs`] the directory depends on: the record's revision, the loaded
/// packages and the updates that saw a change. What the window shows is not in it — the
/// directory answers questions about the whole workspace, so a change that only brings other
/// rows on screen leaves it as it was.
type DirectoryInputs = (u64, Vec<tilolang_tilo::runtime::PackageId>, u64);

/// What the instance publishes on its connection: the workspace's records with their actions, kept up
/// to date change by change.
struct Surface {
    /// The instance's app: its resource carries what every app publishes.
    app: String,
    /// "Show this window", in the person's language.
    activate: tilo_actions::ActionSpec,
    host: Host,
    planner: Arc<dyn Planner>,
    committer: Arc<dyn Committer>,
    publications: RefCell<Publications>,
    /// What schedules a frame when a read on a thread of its own ends.
    wake: Waker,
}

impl Surface {
    fn new(app: &str, activate: tilo_actions::ActionSpec, host: &Host, wake: Waker) -> Self {
        let handle = Arc::new(
            host.handle()
                .with_caller(|| CALLER.with(|caller| caller.borrow().clone())),
        );
        let mut publications = Publications::new(host);
        publications.set_waker(wake.clone());
        // This surface is a window's: it publishes the records the window draws and the
        // resources its invocations name, not the whole workspace. What a caller that asks the
        // directory reads is read from the workspace on demand (`everything`).
        publications.window();
        let planner: Arc<dyn Planner> = handle.clone();
        let surface = Self {
            app: app.to_owned(),
            activate,
            host: host.clone(),
            planner,
            committer: handle,
            publications: RefCell::new(publications),
            wake,
        };
        surface.update();
        surface
    }

    fn update(&self) {
        self.publications
            .borrow_mut()
            .update(&self.host, &self.planner);
    }

    /// What [`Self::resources`] is built from, besides what never changes for this surface: the
    /// published records' revision, the loaded packages, the updates that saw a change, and the
    /// ids of the resources the window shows.
    fn inputs(&self, shown: &[String]) -> SurfaceInputs {
        (
            self.publications.borrow().revision(),
            self.host.package_ids(),
            self.publications.borrow().moved(),
            shown.to_vec(),
        )
    }

    /// The resources the instance publishes: the host's own resource, the workspace's records
    /// with their actions, and each package's actions that take no record (the instance's own
    /// app's with what every app publishes).
    ///
    /// `shown` bounds what a record publishes to what the window reaches: its lane's subject and
    /// the rows on screen. `None` is the whole workspace, what a caller that asks the directory
    /// reads.
    ///
    /// A record is published before the package catalogue: what the window holds and the actions
    /// its own resources offer are what a person came to do, so the palette lists them first, and
    /// the verbs the platform adds for every app (`with_app_actions`) follow the app's own.
    fn resources(&self, shown: Option<&[String]>) -> Vec<ResourceSpec> {
        let mut out = vec![host_resource(&self.committer)];
        let trees = {
            let mut publications = self.publications.borrow_mut();
            match shown {
                Some(shown) => publications.reached(&self.host, &self.planner, &self.app, shown),
                None => publications.everything(&self.host, &self.planner),
            }
        };
        out.extend(trees.into_iter().map(|tree| (*tree).clone()));
        let words = self.host.runtime().words();
        for package in self.host.packages() {
            let mut resource = publish_package(&package.meta, &self.planner, &*words);
            if package.meta.app == self.app {
                resource = with_app_actions(resource, &self.activate);
            }
            if !resource.actions.is_empty() {
                out.push(resource);
            }
        }
        out
    }

    /// What an invocation of `address` resolves against: every app's own actions, and the
    /// resource the address names, published down to it — what a caller that asks of one
    /// resource reads, whatever a window shows.
    fn addressed(&self, shown: &[String], address: &str) -> Vec<ResourceSpec> {
        self.with_addressed(self.resources(Some(shown)), address)
    }

    /// `resources` with what the address names, added as the record publishes it when the window
    /// does not show it: an invocation resolves against the resource it names, whatever the
    /// window shows.
    fn with_addressed(&self, mut resources: Vec<ResourceSpec>, address: &str) -> Vec<ResourceSpec> {
        if resources.iter().any(|resource| holds(resource, address)) {
            return resources;
        }
        if let Some(tree) =
            self.publications
                .borrow_mut()
                .addressed(&self.host, &self.planner, &self.app, address)
            && let Some(spec) = find(&tree, address)
        {
            resources.push(spec);
        }
        resources
    }
}

/// `spec` without its children: the resource itself, its actions and its views.
fn shallow(spec: &ResourceSpec) -> ResourceSpec {
    ResourceSpec {
        id: spec.id.clone(),
        kind: spec.kind.clone(),
        label: spec.label.clone(),
        presentation: spec.presentation.clone(),
        semantic_identity: spec.semantic_identity.clone(),
        app_id: spec.app_id.clone(),
        children: Vec::new(),
        actions: spec.actions.clone(),
        views: spec.views.clone(),
        readers: spec.readers,
        resource_type: spec.resource_type.clone(),
    }
}

/// `spec` or a resource under it, its address `address`: without children, which no address
/// resolves through.
fn find(spec: &ResourceSpec, address: &str) -> Option<ResourceSpec> {
    if spec.id.as_str() == address {
        return Some(shallow(spec));
    }
    spec.children.iter().find_map(|child| find(child, address))
}

/// Whether `spec` or a resource under it is `address`.
fn holds(spec: &ResourceSpec, address: &str) -> bool {
    spec.id.as_str() == address || spec.children.iter().any(|child| holds(child, address))
}

/// The instance's hold on its surface: while the instance publishes, it is the instance's.
struct Held(Rc<Surface>);

impl Publication for Held {
    fn update(&self) {
        self.0.update();
    }

    fn pending(&self) -> bool {
        self.0.publications.borrow().is_building()
    }

    /// `Publications::settle` waits for each record being read off the frame, and reads again
    /// what it took in: what a window of a host without frames reads once it covers the record.
    fn settle(&self) -> bool {
        let mut publications = self.0.publications.borrow_mut();
        !publications
            .settle(&self.0.host, &self.0.planner)
            .is_empty()
    }

    fn visited(&self) -> u64 {
        self.0.publications.borrow().visited()
    }

    fn sweeps(&self) -> u64 {
        self.0.publications.borrow().sweeps()
    }
}

/// `resource` with what every app publishes, those it does not offer itself: the history verbs
/// of its record, `read` and "Show this window".
fn with_app_actions(resource: ResourceSpec, activate: &tilo_actions::ActionSpec) -> ResourceSpec {
    let mut resource = tilolang_tilo::history::with_verbs(resource);
    for spec in [tilo_resource::read::spec(None), activate.clone()] {
        if !resource
            .actions
            .iter()
            .any(|existing| existing.id == spec.id)
        {
            resource = resource.with_action(spec);
        }
    }
    resource
}

type SurfaceSlot = Rc<RefCell<Option<Rc<Surface>>>>;

/// The person's formats, as the session handed them over the host's door: shared by every
/// instance the host runs, so a change in Settings reaches them together.
pub type Formats = Rc<Cell<tilo_formats::RegionFormats>>;

/// What the instance asks of the host: its package, the platform's phrases and clock, waking the
/// host, and publishing on its connection.
struct HostPlatform {
    bundle: Arc<Bundle>,
    /// The apps installed here ([`Spec::installed`]): a package the app uses that is one of them
    /// is loaded as another app's.
    installed: BTreeMap<String, String>,
    /// The words of this window: its app's own and its frames', in the language the window speaks
    /// (the person's where the package ships it), so what its dates and clocks read in is the same
    /// language the toolkit's own say them in.
    phrases: Rc<Phrases>,
    /// The person's formats, which their clock and dates read by.
    formats: Formats,
    wake: Waker,
    surface: SurfaceSlot,
}

impl Platform for HostPlatform {
    fn load(&self, host: &Host) -> u64 {
        if let Err(error) =
            host.load_bundle_using(&self.bundle, |app| self.installed.contains_key(app))
        {
            tracing::error!(app = self.bundle.meta.app, %error, "the package did not load");
        }
        0
    }

    fn phrase(&self, id: &str, args: &[(String, String)]) -> String {
        self.phrases.say(id, args)
    }

    fn language(&self) -> String {
        self.phrases.language()
    }

    fn add_words(&self, window: bool, catalogues: &[(String, String)]) {
        let _ = window;
        self.phrases.add_package(catalogues);
    }

    fn now(&self) -> String {
        local_now()
            .with_timezone(&chrono::Utc)
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string()
    }

    /// The zone the person's clock stands in, which their day begins in: the device's, as their
    /// window's dates read it, so a day an action writes is the day their window shows it under.
    fn zone(&self) -> i64 {
        i64::from(local_now().offset().local_minus_utc())
    }

    /// The person's zone by name, so a day begins and a repeating item keeps its hour across a
    /// change of clocks, as their window's dates do.
    fn zone_name(&self) -> Option<String> {
        tilo_clock::local_zone_name()
    }

    /// The day the person's region starts its week on, as they chose it in Settings: the same
    /// first day their window's dates start a week on.
    fn week_start(&self) -> i64 {
        i64::from(
            self.formats
                .get()
                .first_day
                .weekday()
                .num_days_from_monday(),
        )
    }

    /// The clock the person reads: the one they chose in Settings, which the session handed the
    /// host, so a time an app writes (`a.fetched.clock()`) reads as the same clock its window shows.
    fn clock(&self, hour: u32, minute: u32) -> String {
        self.formats
            .get()
            .time
            .format_hm(hour, minute, &*self.phrases)
    }

    /// A day or a time of day in a sentence, in the person's styles and their language: the zone
    /// its times read in is the one this host's clock answers in, so a sentence and a view agree.
    fn shown(&self, kind: &str, value: &str) -> String {
        let now = local_now();
        tilo_formats::shown(
            kind,
            value,
            self.formats.get(),
            now.date_naive(),
            now.naive_local(),
            i64::from(now.offset().local_minus_utc()),
            &*self.phrases,
        )
    }

    fn web(
        &self,
        request: &tilolang_tilo::runtime::run::WebRequest,
    ) -> tilolang_tilo::runtime::web::WebReply {
        crate::web::fetch(request)
    }

    fn read_content(&self, hash: &str) -> Option<Vec<u8>> {
        crate::files::read(hash)
    }

    fn choose_file(
        &self,
        request: &tilolang_tilo::runtime::blob::FileRequest,
    ) -> Result<Option<tilolang_tilo::runtime::blob::FileValue>, String> {
        crate::files::choose(request)
    }

    fn save_file(
        &self,
        request: &tilolang_tilo::runtime::blob::SaveRequest,
        content: &[u8],
    ) -> Result<(), String> {
        crate::files::save(request, content)
    }

    fn keep_content(
        &self,
        name: &str,
        media: &str,
        bytes: &[u8],
    ) -> Result<tilolang_tilo::runtime::blob::FileValue, String> {
        crate::files::keep(name, media, bytes)
    }

    fn waker(&self) -> Waker {
        Arc::clone(&self.wake)
    }

    fn publish(&self, host: &Host, wake: Waker) -> Option<Box<dyn Publication>> {
        let activate = tilo_actions::remote::activate_spec(
            self.phrases.say("app-activate-window", &[]),
            self.phrases.say("app-activate-window-description", &[]),
        );
        let surface = Rc::new(Surface::new(&self.bundle.meta.app, activate, host, wake));
        *self.surface.borrow_mut() = Some(Rc::clone(&surface));
        Some(Box::new(Held(surface)))
    }
}

/// Where an instance keeps what it keeps: its workspace record's on-disk copy and its indexes,
/// under its package's directory in the host's storage.
pub struct Kept {
    pub documents: Rc<dyn durable_store::DurableStore>,
    pub indexes: Option<std::path::PathBuf>,
}

/// The directory of the indexes of `record` of `workspace` under `indexes`: one per record, read by
/// every window of it ([`tilolang_tilo::index::Held`]). A window opened again reads what the last
/// one kept. Copies an earlier build kept per window beside it are derived and go.
fn record_indexes(
    indexes: &std::path::Path,
    workspace: &str,
    record: Option<&str>,
) -> std::path::PathBuf {
    let dir = tilolang_tilo::index::record_dir(indexes, workspace, record);
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy().starts_with("instance-")
                && let Err(error) = std::fs::remove_dir_all(entry.path())
            {
                tracing::warn!(path = %entry.path().display(), %error, "a window's own index copy was not removed");
            }
        }
    }
    dir
}

/// What the actions of the rows a window shows answered, by resource and action: a window asks
/// about every action of every row it shows, and the rows it shows move as it scrolls.
#[derive(Default)]
struct Answers {
    /// The answer to each question, by the resource and the action it was asked of.
    said: std::collections::HashMap<(String, String), Answered>,
    /// The loaded packages the answers were read over: a package that loads is answered again.
    packages: Vec<tilolang_tilo::runtime::PackageId>,
}

/// What one action of one published resource answered, and what reading it read.
struct Answered {
    /// Where the publications stood when the answer was read ([`Publications::moved`]).
    at: u64,
    /// What the app says of the action on the resource: what it is called there, the icon beside
    /// it and whether it can run; `None` for an action whose app says nothing its subject
    /// decides, which is offered as it is published.
    said: Option<tilolang_tilo::host::Offered>,
    /// The fields reading the answer read; `None` for an answer that read beyond the workspace
    /// (the clock, the window's space, the person's view state), which moves without it moving.
    reads: Option<Vec<(String, String)>>,
}

/// A row that shows what an app says of an action on it: what it is called there, the icon
/// beside it and whether it can run. The window's catalogue and the action directory carry the
/// same answer, so both are shown it the same way.
trait Shows {
    fn shows(&mut self, offered: &tilolang_tilo::host::Offered);
}

impl Shows for ActionDescriptor {
    fn shows(&mut self, offered: &tilolang_tilo::host::Offered) {
        if let Some(label) = &offered.label {
            self.action_label.clone_from(label);
        }
        if let Some(icon) = &offered.icon {
            self.icon = Some(icon.clone());
        }
        if let Some(availability) = &offered.availability {
            self.contract.availability = availability.clone();
        }
    }
}

impl Shows for collaboration::ExportedAction {
    fn shows(&mut self, offered: &tilolang_tilo::host::Offered) {
        if let Some(label) = &offered.label {
            self.action_label.clone_from(label);
        }
        if let Some(icon) = &offered.icon {
            self.icon = Some(icon.clone());
        }
        if let Some(availability) = &offered.availability {
            self.contract.availability = availability.clone();
        }
    }
}

/// How many answers a window's rows keep: a person scrolling a list of ten thousand asks about
/// the rows they reach, so keeping every answer of every row would grow with the workspace.
/// Past this, the answers start over.
const ANSWERS_KEPT: usize = 8_192;

impl Answers {
    /// What the action `action` offers on `resource`, as `host` says it now, with the
    /// publications where `at` says: an answer read earlier stands while nothing it read moved
    /// ([`Self::forget`]).
    fn answer(
        &mut self,
        host: &Host,
        at: u64,
        resource: &str,
        action: &str,
    ) -> Option<tilolang_tilo::host::Offered> {
        let key = (resource.to_owned(), action.to_owned());
        if let Some(answered) = self.said.get(&key) {
            return answered.said.clone();
        }
        let (said, reads) = match host.offered_reading(resource, action) {
            Some((offered, reading)) => (Some(offered), (!reading.beyond).then_some(reading.reads)),
            None => (None, Some(Vec::new())),
        };
        if self.said.len() >= ANSWERS_KEPT {
            self.said.clear();
        }
        self.said.insert(
            key,
            Answered {
                at,
                said: said.clone(),
                reads,
            },
        );
        said
    }

    /// Forget what a change reached since an answer was read ([`Publications::changed_since`]),
    /// and every answer when the packages loaded changed: an answer read nothing but the
    /// workspace, so it stands until a field it read moves. Whether a change reached one, which
    /// is what a surface offering the actions must read again.
    fn forget(
        &mut self,
        publications: &Publications,
        packages: &[tilolang_tilo::runtime::PackageId],
    ) -> bool {
        if packages != self.packages.as_slice() {
            self.packages = packages.to_vec();
            self.said.clear();
            return true;
        }
        let mut since: std::collections::HashMap<u64, Option<Moved>> =
            std::collections::HashMap::new();
        let mut moved = false;
        self.said.retain(|_, answered| {
            let Some(reads) = &answered.reads else {
                return false;
            };
            let reached_now = match since
                .entry(answered.at)
                .or_insert_with(|| publications.changed_since(answered.at))
            {
                None | Some(Moved::Everything) => true,
                Some(Moved::These(changed)) => changed
                    .iter()
                    .any(|changed| reads.iter().any(|read| reached(changed, read))),
            };
            moved |= reached_now;
            !reached_now
        });
        moved
    }
}

/// One app instance. See the module docs.
pub struct Hosted {
    spec: Spec,
    /// The app's package: what a read of a past version runs.
    bundle: Arc<Bundle>,
    /// The app's resource, by its id and kind: where its history verbs are published.
    resource: (String, String),
    instance: Instance,
    /// The words of the window, which the instance's platform reads: the person's language
    /// chosen again ([`Self::speak`]) relabels the window where it stands.
    phrases: Rc<Phrases>,
    record: InstanceRecord,
    surface: SurfaceSlot,
    /// What the broker holds of the instance's surface, and the descriptors it was built from.
    published: Option<(Vec<ActionDescriptor>, SurfacePublication)>,
    /// The directory the instance answers its connection's directory requests from: what it
    /// last published, browsable by level and name, as the door and agents read it.
    directory: PublishedDirectory,
    /// Whether the surface moved since [`Self::directory`] was built ([`Self::publish`],
    /// [`Self::relabel`]): it is built again when it is next asked, not on every change.
    directory_behind: bool,
    /// What [`Self::directory`] was built from ([`DirectoryInputs`]): a publication whose
    /// inputs it covers leaves the directory as it was, so a workspace of ten thousand items
    /// surveys its door once, not once per row scrolled onto the screen.
    directory_from: Option<DirectoryInputs>,
    /// The surface and its inputs ([`Surface::inputs`]) the broker's copy was built from: while
    /// they are the same, a turn publishes without building anything.
    published_from: Option<(Rc<Surface>, SurfaceInputs)>,
    /// Why the surface was last refused instead of published: empty once it publishes.
    refused: Vec<tilo_actions::ContractViolation>,
    /// What can run on the rows the window shows, and what reading each answer read: a scroll or
    /// a place switch asks again only for the rows that came into view, and only when what their
    /// answers read moved ([`Answers`]).
    answers: Answers,
    registry: ActionRegistry,
    /// What the instance last published to the session's persistent search index: it publishes
    /// the entries of the resources the publication moved, so a Mail message is found by a word
    /// of its body through the system's one search, whether or not the window runs.
    search_index: tilo_resource::search_index::Publisher,
    /// Where the publication's journal stood when the index last took it: what the change since
    /// moved, so the index reads the resources it moved and not the workspace. [`None`] until it
    /// has published once.
    search_index_moved: Option<u64>,
    /// The entry each resource has in the index, by its published id: what a change replaces,
    /// and what a resource that is gone takes away, its own entries and those under it.
    search_entries: std::collections::BTreeMap<String, tilo_resource::search_index::IndexEntry>,
    /// How many builds of the entries the index has been sent: the state's generation.
    search_index_build: u64,
    /// The whole of the index being read off the frame: what the publication had moved when it
    /// was read, and the read. The frame after it ends sends it.
    search_index_whole: Option<(u64, tilolang_tilo::search::WholeIndex)>,
    /// What wakes the host: an index read off the frame wakes it too, and the read is the first
    /// frame's, before the instance published a surface with a wake of its own.
    wake: Waker,
    /// The invocation the person's last step ran in, when they asked to undo it: the host takes
    /// the action back in every unit that holds a change of it, all under one commit, whether
    /// this endpoint's logs still remember the action or only the records do.
    undo_ask: Option<InvocationId>,
    /// The other apps the app uses, each read from its own record ([`Used`]).
    used: Vec<Used>,
    /// What the app's views and actions asked of the other apps' doors, waiting to be sent.
    asks: Rc<RefCell<Vec<tilolang_tilo::runtime::delegate::Invocation>>>,
    /// Whether the runtime sends what it changes of another app's resources to [`Self::asks`].
    asking: bool,
    /// The identity-service connection the instance's connection signs through, held for as
    /// long as the instance runs.
    #[allow(
        dead_code,
        reason = "held open, never read: the connection signs through it"
    )]
    identity: Option<std::os::fd::OwnedFd>,
}

impl Hosted {
    /// The instance `spec` of `bundle`, over the workspace record its connection `manager` joined,
    /// kept in `kept`, sending its window patches over `window`.
    pub fn open(
        spec: Spec,
        manager: collaboration::CollaborationManager,
        bundle: Arc<Bundle>,
        kept: &Kept,
        window: Box<dyn Transport>,
        phrases: &Rc<Phrases>,
        formats: &Formats,
        wake: Waker,
    ) -> Result<Self, RecordError> {
        let record = InstanceRecord::open(manager, &kept.documents, spec.person.clone())?;
        Ok(Self::with_record(
            spec, record, bundle, kept, window, phrases, formats, wake,
        ))
    }

    /// The instance `spec` of `bundle` over `record`, which it holds from now on.
    #[must_use]
    pub fn with_record(
        spec: Spec,
        record: InstanceRecord,
        bundle: Arc<Bundle>,
        kept: &Kept,
        window: Box<dyn Transport>,
        phrases: &Rc<Phrases>,
        formats: &Formats,
        wake: Waker,
    ) -> Self {
        let surface: SurfaceSlot = Rc::default();
        let resource = (
            package_resource_id(&bundle.meta.app),
            bundle.meta.package_kind(),
        );
        // The window speaks the person's language where its package ships it, and the package's
        // source language otherwise: the frames' words, the app's own words and the history's
        // sentences all follow that language, never the one this host's process runs in.
        let phrases = Rc::new(window_phrases(phrases, &spec, &bundle));
        // The names the session sends for the apps installed here read in the language this
        // window speaks, whatever language the workspace's people are named in.
        collaboration::history_source::authors::set_names_language(&phrases.locale());
        let mut record = record.speaking(phrases.locale());
        let used = used_apps(&spec, &bundle, &mut record);
        let platform = HostPlatform {
            bundle: Arc::clone(&bundle),
            installed: spec.installed.clone(),
            phrases: Rc::clone(&phrases),
            formats: Rc::clone(formats),
            wake: Arc::clone(&wake),
            surface: Rc::clone(&surface),
        };
        let mut instance = Instance::new(
            Config {
                app: spec.app.clone(),
                me: spec.person.clone(),
                mode: Mode::Live,
                device: None,
                index_dir: kept.indexes.as_ref().map(|indexes| {
                    record_indexes(indexes, &spec.workspace, record.route().record.as_deref())
                }),
                record_loader: None,
                root: tilolang_tilo::host::RootView::default(),
                sink: None,
            },
            Rc::new(platform),
            // People by what the window calls them, once it said (`ToApp::Names`).
            Rc::new(tilolang_tilo::names::Unnamed),
            window,
        );
        // The device that governs the workspace is its records' origin: the package's migrations
        // run there, once.
        instance.set_origin(record.governs());
        let registry = ActionRegistry::with(tilolang_tilo::history::register);
        Self {
            spec,
            bundle,
            resource,
            instance,
            phrases,
            record,
            surface,
            published: None,
            directory: PublishedDirectory::default(),
            directory_behind: false,
            directory_from: None,
            published_from: None,
            refused: Vec::new(),
            answers: Answers::default(),
            registry,
            search_index: tilo_resource::search_index::Publisher::default(),
            search_index_moved: None,
            search_entries: std::collections::BTreeMap::new(),
            search_index_build: 0,
            search_index_whole: None,
            wake,
            undo_ask: None,
            used,
            asks: Rc::default(),
            asking: false,
            identity: None,
        }
    }

    /// Show `anchor` in the window, as a search hit or an agent's `open` of it does.
    pub fn reveal(&mut self, anchor: String) {
        self.instance
            .open_requests()
            .borrow_mut()
            .push(OpenEffect::Open { anchor });
    }

    /// What the broker was last handed of the instance's actions.
    #[must_use]
    pub fn published(&self) -> &[ActionDescriptor] {
        self.published
            .as_ref()
            .map_or(&[], |(descriptors, _)| descriptors.as_slice())
    }

    /// Whether the whole of the workspace's search index is still being read off the frame: the
    /// frame that finds it read sends it.
    #[must_use]
    pub fn index_pending(&self) -> bool {
        self.search_index_whole.is_some()
    }

    /// Why the instance's surface was last refused rather than published: empty while what it
    /// offers is a valid resource/action surface.
    #[must_use]
    pub fn refused(&self) -> &[tilo_actions::ContractViolation] {
        &self.refused
    }

    /// Keep the identity-service connection the instance's connection signs through for as long
    /// as the instance runs.
    #[must_use]
    pub fn holding_identity(mut self, identity: std::os::fd::OwnedFd) -> Self {
        self.identity = Some(identity);
        self
    }

    /// What the instance is.
    #[must_use]
    pub fn spec(&self) -> &Spec {
        &self.spec
    }

    /// The instance's runtime and views.
    #[must_use]
    pub fn instance(&self) -> &Instance {
        &self.instance
    }

    /// The workspace record the instance holds.
    #[must_use]
    pub fn record(&self) -> &InstanceRecord {
        &self.record
    }

    /// The package the instance runs.
    #[must_use]
    pub fn bundle(&self) -> &Bundle {
        &self.bundle
    }

    /// Keep the package's types with an origin in step with `origin`'s service.
    pub fn add_origin(&mut self, origin: tilolang_tilo::origin::SharedOrigin) {
        self.instance.add_origin(origin);
    }

    /// The person's environment changed — their formats, or the name the window calls someone:
    /// every view that shows what only the window knows runs again, and its window shows it.
    pub fn person_changed(&mut self) {
        self.instance.person_changed();
    }

    /// The person chose `language` (`de-DE`) in Settings: the window reads its words in it where
    /// its package ships it, at once and where it stands — its views, the labels it publishes and
    /// its history's sentences. Whether its words read in another language now.
    pub fn speak(&mut self, language: &str) -> bool {
        if !self.phrases.speak(language) {
            return false;
        }
        let locale = self.phrases.locale();
        collaboration::history_source::authors::set_names_language(&locale);
        self.record.speak(locale.clone());
        // Every word is read again: the views drawn, the window's own chrome and the surface
        // published.
        self.instance.speak(locale);
        self.published_from = None;
        self.answers = Answers::default();
        true
    }

    /// Run the package's scenario `scenario` in the instance on its next frame, as a capture
    /// stages one: its changes are the person's steps on the instance's record.
    pub fn stage(&mut self, scenario: &str) {
        self.instance.stage(Some(scenario));
    }

    /// One frame: what the workspace sent taken in, the runtime brought up to date with the record,
    /// what the person did run, what was planned committed and sent, what the broker delivered
    /// run, and the surface published. Whether anything changed.
    pub fn turn(&mut self) -> bool {
        let mut events = Vec::new();
        let mut changed = self
            .record
            .receive_and(&mut self.instance, |event| events.push(event));
        // The chunks the replica holds with the record's own frame: the store reads them as what
        // the window shows reaches into them, and the indexes are built over them.
        self.instance.sync_chunks(self.record.chunks());
        changed |= self.instance.update();
        let chose = self.chosen();
        changed |= chose;
        if let Some(why) = self.instance.take_stage_failure() {
            tracing::error!(%why, "the staged scenario failed");
        }
        changed |= self.commit();
        if chose {
            self.instance.show_ran();
        }
        // After what the person did before asking is committed: an undo takes back their last
        // step, typing they did just before it included.
        for asked in self.instance.take_history() {
            if matches!(asked, tilolang_tilo::instance::HistoryAsk::Undo)
                && let Some(invocation) = self.record.last_step_invocation()
            {
                // The units that hold a change of the step's action take it back together,
                // so the host does it, once, and publishes the reverts as one action.
                self.undo_ask = Some(invocation);
                continue;
            }
            changed |= self.take_step(&asked);
        }
        for event in events {
            changed |= self.delivered(event);
        }
        changed |= self.read_used();
        self.send_asks();
        self.publish();
        self.answer_directory();
        self.publish_search_index();
        changed
    }

    /// Commit what the runtime planned and send it on the route. Whether anything was.
    fn commit(&mut self) -> bool {
        match self.record.commit(&mut self.instance) {
            Ok(committed) => !committed.is_empty(),
            Err(error) => {
                tracing::warn!(app = self.spec.app, %error, "a change did not reach the workspace");
                false
            }
        }
    }

    /// One history ask run through the record: the person's step taken back, brought back, or
    /// answered with why it cannot be. Whether what the instance holds changed.
    fn take_step(&mut self, asked: &tilolang_tilo::instance::HistoryAsk) -> bool {
        let (resource, kind) = &self.resource;
        match self.record.asked(&mut self.instance, asked, resource, kind) {
            Ok(done) => done,
            Err(error) => {
                tracing::warn!(app = self.spec.app, ?asked, %error, "an undo or redo failed");
                self.instance.history_failed(error.to_string());
                true
            }
        }
    }

    /// The invocation of the step the person asked to undo, once: the host takes the action back
    /// in every unit of the workspace that holds a change of it.
    pub fn take_undo_ask(&mut self) -> Option<InvocationId> {
        self.undo_ask.take()
    }

    /// Take the step the person asked to undo back in this unit alone, when no other unit of
    /// the workspace holds a change of its action.
    pub fn undo_here(&mut self) -> bool {
        self.take_step(&tilolang_tilo::instance::HistoryAsk::Undo)
    }

    /// The changes of this instance's unit the invocation `invocation` made: the unit's part of
    /// the action, read from its record.
    #[must_use]
    pub fn changes_of(&self, invocation: InvocationId) -> Vec<[u8; 32]> {
        self.record.changes_of(invocation)
    }

    /// Whether this instance holds the step the change `change` is part of, still to take back:
    /// what the person's undo of it takes back.
    #[must_use]
    pub fn can_revert(&self, change: &[u8; 32]) -> bool {
        self.record.can_revert(change)
    }

    /// Take back the step the change `change` is part of in this instance, as a step of the
    /// person in `ambient`, and leave it unsent: the host sends every unit of the action under
    /// one commit. The instance shows what it now holds.
    ///
    /// # Errors
    /// The step is undone already, is not this person's to take back, or its inverse could not
    /// be applied.
    pub fn revert_step(
        &mut self,
        change: &[u8; 32],
        ambient: &ActionContext,
    ) -> Result<(), RecordError> {
        let (resource, kind) = &self.resource;
        let invoker = ActionInvoker::actor(self.spec.person.clone());
        self.record
            .revert_step(&mut self.instance, resource, kind, invoker, ambient, change)
    }

    /// What this instance holds that the workspace was not handed yet, as its parts
    /// ([`Self::share_under`]): one per chunk an action wrote, each named by the record it travels
    /// on. Empty when the workspace holds all of it.
    pub fn unsent_parts(&self) -> Vec<(String, DocumentMutation)> {
        self.record
            .unsent_parts()
            .into_iter()
            .filter(|(_, change)| match change {
                DocumentMutation::Incremental(bytes) => !bytes.is_empty(),
                DocumentMutation::Snapshot(_) => true,
            })
            .map(|(chunk, change)| {
                (
                    self.record.chunk_route(chunk).record.unwrap_or_default(),
                    change,
                )
            })
            .collect()
    }

    /// Send this instance's unsent change on its own: the workspace takes it as this unit's
    /// action. What the workspace refused for good, it says to the window.
    pub fn share(&mut self) {
        let unshared = self.record.share();
        self.took_share(unshared);
    }

    /// Send this instance's unsent change as its part of `commit`, the action across every unit
    /// it wrote. What the workspace refused for good, it says to the window.
    pub fn share_under(&mut self, commit: &WorkspaceCommit) {
        let unshared = self.record.share_under(commit);
        self.took_share(unshared);
    }

    fn took_share(&mut self, unshared: Option<String>) {
        if let Some(why) = unshared {
            self.instance.unshared(why);
        }
        self.instance.sync_chunks(self.record.chunks());
    }

    /// Publish the app's resources to the session's persistent search index, over the
    /// instance's certified route: what search finds of the room (a note's title, a message's
    /// sender, subject and body), keyed by each resource's id, so a hit opens the app at it.
    /// Nothing is sent while nothing moved; a new route sends the whole set again, and so does a
    /// change the publication's journal no longer reaches. Otherwise a change sends the entries
    /// of the resources it moved, and drops those of the resources it deleted: a task checked in
    /// a book of ten thousand replaces one entry.
    fn publish_search_index(&mut self) {
        // A whole index read off the frame this turn is taken in and sent: its entries were the
        // workspace's to read, not the frame's.
        if self.index_read_off_frame() {
            return;
        }
        if self.record.route().app_id != self.spec.app {
            return;
        }
        let Some(host) = self.instance.host() else {
            return;
        };
        sync_record(&host, &self.record);
        // An index still being built off the frame holds nothing yet: the frame that takes it
        // in publishes what it holds, which is why nothing is marked as sent here.
        if !host.index_ready() {
            return;
        }
        let Some(surface) = self.surface.borrow().clone() else {
            return;
        };
        // What the workspace moved since the index last took it, read from the publication the
        // window reads rather than from the record: a change becomes a publication at the frame
        // that reads it from the store, and the index publishes that frame's changes, not the
        // ones before.
        surface.update();
        // A publication no earlier frame took ([`Self::search_index_moved`] is `None`) has no
        // journal to read from: the whole index is read off the frame, as it is when the
        // journal no longer reaches back to it.
        let moved = self
            .search_index_moved
            .and_then(|since| surface.publications.borrow().text_since(since));
        // What a hit's address names opens at its resource; the entries carry the package's
        // `open`. A change sends the entries it moved; a whole publication reads the workspace
        // off the frame instead.
        let Some(tilolang_tilo::publish::TextMoves::These { texts, gone }) = moved else {
            self.read_index_whole(&host);
            return;
        };
        if texts.is_empty() && gone.is_empty() {
            return;
        }
        let state = self.index_state();
        let (upsert, remove) = self.moved_entries(&host, &texts, &gone);
        let Some(deltas) = self.search_index.publish_moved(state, upsert, remove) else {
            self.read_index_whole(&host);
            return;
        };
        for delta in deltas {
            let Some(bytes) = delta.encode() else {
                continue;
            };
            self.record
                .manager_mut()
                .send(collaboration::CollabRequest::PublishSearchIndex { delta_json: bytes });
        }
        self.search_index_moved = Some(surface.publications.borrow().moved());
    }

    /// The state the entries are published at: a generation of its own for every publication.
    fn index_state(&mut self) -> tilo_resource::search_index::IndexState {
        self.search_index_build = self.search_index_build.saturating_add(1);
        tilo_resource::search_index::IndexState {
            generation: self.search_index_build,
            secrets_unlocked: None,
        }
    }

    /// Read every entry of the workspace off the frame: what a first publication, a new route
    /// and a change the journal no longer reaches send. Nothing is sent this turn; the frame the
    /// read ends on sends it ([`Self::index_read_off_frame`]).
    fn read_index_whole(&mut self, host: &Host) {
        let publisher = std::mem::take(&mut self.search_index);
        // The instance's surface wakes the host while it runs; before it published one, the
        // host's own waker does, so a read that ends with nothing else to do still has its frame
        // taken.
        let wake = Some(
            self.surface
                .borrow()
                .as_ref()
                .map_or_else(|| self.wake.clone(), |surface| surface.wake.clone()),
        );
        let state = self.index_state();
        // The publication as it stands now: what a change from here on moves is published by
        // the frames after the read, not read with it.
        let moved = self.moved();
        self.search_index_whole = Some((
            moved,
            tilolang_tilo::search::WholeIndex::start(host, publisher, state, wake),
        ));
    }

    /// Take in the whole index read off the frame and send it: whether it ended this turn, which
    /// leaves the frame with nothing else to publish. The thread that read it ended without an
    /// answer only by panicking, and then the frame reads it.
    fn index_read_off_frame(&mut self) -> bool {
        let Some((moved, read)) = self.search_index_whole.take() else {
            return false;
        };
        let published = match read.poll() {
            tilolang_tilo::background::Poll::Running => {
                self.search_index_whole = Some((moved, read));
                return true;
            }
            tilolang_tilo::background::Poll::Ready(published) => published,
            tilolang_tilo::background::Poll::Lost => {
                let Some(host) = self.instance.host() else {
                    return true;
                };
                sync_record(&host, &self.record);
                tilolang_tilo::search::read(
                    &host.store().snapshot(),
                    &tilolang_tilo::search::catalogue(&host),
                    std::mem::take(&mut self.search_index),
                    tilo_resource::search_index::IndexState {
                        generation: self.search_index_build,
                        secrets_unlocked: None,
                    },
                )
            }
        };
        self.search_entries = published.entries;
        self.search_index = published.publisher;
        for bytes in published.deltas {
            self.record
                .manager_mut()
                .send(collaboration::CollabRequest::PublishSearchIndex { delta_json: bytes });
        }
        self.search_index_moved = Some(moved);
        true
    }

    /// Fill what can run now on the resources the window shows ([`Instance::shown_resources`]):
    /// the rows its palette offers are the actions on what it draws, and a condition is the
    /// app's to answer for them. Every other row is answered when a caller asks about it
    /// ([`Self::answer_directory`]), so nothing carries a condition's answer for every item a
    /// query could name.
    ///
    /// An answer stands while nothing it read moved ([`Answers`]): a scroll shows rows whose
    /// answers are known, and costs only the rows that came into view.
    fn answer_what_the_window_shows(&mut self, descriptors: &mut [ActionDescriptor]) {
        let Some(host) = self.instance.host() else {
            return;
        };
        let shown = self.instance.shown_resources();
        self.forget_answers();
        let at = self.moved();
        for descriptor in descriptors {
            if !shown.contains(&descriptor.address.resource) {
                continue;
            }
            self.show_offer(&host, at, descriptor);
        }
    }

    /// What the app says of `descriptor`'s action on its resource now, as the window shows it:
    /// what it is called there, the icon beside it, and whether it can run ([`Answers`]).
    fn show_offer(&mut self, host: &Host, at: u64, descriptor: &mut ActionDescriptor) {
        let Some(offered) = self.answers.answer(
            host,
            at,
            &descriptor.address.resource,
            &descriptor.address.action,
        ) else {
            return;
        };
        descriptor.shows(&offered);
    }

    /// Where the publications stand now: what an answer is read at, and how far it stands
    /// without being read again.
    fn moved(&self) -> u64 {
        self.surface
            .borrow()
            .as_ref()
            .map_or(0, |surface| surface.publications.borrow().moved())
    }

    /// Forget the answers a change since reached ([`Answers::forget`]): what a window shows is
    /// answered again only where what it read moved, and rows no change reached keep theirs.
    /// Whether a change reached one, which is what a surface offering the actions reads again.
    fn forget_answers(&mut self) -> bool {
        let surface = self.surface.borrow().clone();
        let Some(surface) = surface else {
            return false;
        };
        let publications = surface.publications.borrow();
        let packages = surface.host.package_ids();
        self.answers.forget(&publications, &packages)
    }

    /// The entries of the resources the change moved, and the keys of the entries it deleted:
    /// a resource read from the store as it is now, each app's own resources by its own package.
    fn moved_entries(
        &mut self,
        host: &Host,
        texts: &std::collections::BTreeSet<String>,
        gone: &std::collections::BTreeSet<String>,
    ) -> (Vec<tilo_resource::search_index::IndexEntry>, Vec<String>) {
        let mut upsert = Vec::new();
        let mut remove = Vec::new();
        let packages = host.packages();
        for path in texts {
            for package in &packages {
                let entry = tilolang_tilo::search::index_entry_at(host, &package.meta, path);
                if let Some(entry) = entry {
                    self.search_entries
                        .insert(entry.resource.clone(), entry.clone());
                    upsert.push(entry);
                }
            }
        }
        // The resources that are gone, and whatever was published under them.
        for package in &packages {
            for path in gone {
                let id = tilolang_tilo::publish::resource_id(&package.meta.app, path);
                self.drop_entry(&id, &mut remove);
            }
        }
        // A resource that holds no text any more is no entry, though it is still there: only its
        // own entry goes, as what it published under it is the entries of its own resources.
        for package in &packages {
            let upserted: std::collections::BTreeSet<&str> =
                upsert.iter().map(|entry| entry.resource.as_str()).collect();
            for path in texts {
                let id = tilolang_tilo::publish::resource_id(&package.meta.app, path);
                if !upserted.contains(id.as_str()) && self.search_entries.remove(&id).is_some() {
                    remove.push(id);
                }
            }
        }
        (upsert, remove)
    }

    /// Drop the index entry of `id`, and those of the resources published under it.
    fn drop_entry(&mut self, id: &str, remove: &mut Vec<String>) {
        let under = format!("{id}/");
        let held: Vec<String> = self
            .search_entries
            .range(id.to_owned()..)
            .take_while(|(key, _)| key.as_str() == id || key.starts_with(&under))
            .map(|(key, _)| key.clone())
            .collect();
        for key in held {
            self.search_entries.remove(&key);
            remove.push(key);
        }
    }

    /// Offer the instance's action directory on its connection, once it is connected, and
    /// answer what was asked of it since the last frame from what the instance published.
    fn answer_directory(&mut self) {
        let manager = self.record.manager_mut();
        manager.publish_action_directory(self.spec.app.clone());
        let requests = manager.take_action_directory_requests();
        if requests.is_empty() {
            return;
        }
        if self.directory_behind {
            self.catch_up_directory();
        }
        // What an action can run for is its app's to say, and it says it for the rows a caller
        // asks about: the surface carries what exists and what it offers, and a requirement's
        // answer only for what the window shows.
        let Some(host) = self.instance.host() else {
            for request in requests {
                self.directory.answer(request);
            }
            return;
        };
        self.forget_answers();
        let at = self.moved();
        let answers = &mut self.answers;
        for request in requests {
            self.directory.answer_refreshed(request, |row| {
                let Some(descriptor) = row.descriptor.as_mut() else {
                    return;
                };
                if let Some(offered) =
                    answers.answer(&host, at, &descriptor.resource, &descriptor.action)
                {
                    descriptor.shows(&offered);
                }
            });
        }
    }

    /// Build the directory from what the instance publishes now, when it is first asked after
    /// the surface moved.
    fn catch_up_directory(&mut self) {
        self.directory_behind = false;
        let Some(surface) = self.surface.borrow().clone() else {
            return;
        };
        // The directory answers what a caller asks of the whole workspace, not of the rows a
        // window shows: it is built from every resource when it is asked, and surveyed with
        // them. The surface the broker holds is the window's, which is not the same set.
        let resources = surface.resources(None);
        let descriptors = tilo_actions::door::survey(&resources);
        self.directory.publish(&resources, &descriptors);
    }

    /// Run what the broker delivered on the instance's connection. Every event that waits on an
    /// answer from the instance is answered: an invocation with its outcome, a handoff step with
    /// what the host does of it. The rest says something the instance does not keep (presence,
    /// cursors, results of requests it never makes), but for what it names: what each app
    /// installed here is called rides with the context, and is what this window's history reads
    /// another app's change as.
    fn delivered(&mut self, event: CollabEvent) -> bool {
        for used in &mut self.used {
            if let Some(moved) = used.record.take(&event, self.record.manager_mut()) {
                used.moved |= moved;
                return false;
            }
        }
        if let CollabEvent::Context { apps, .. } = &event {
            // Apps installed or removed since the last frame: what the views show of who made a
            // change is published again.
            if collaboration::history_source::authors::set_installed_apps(apps.clone()) {
                self.instance.person_changed();
            }
            return true;
        }
        let (id, invocation, resource, action, argument, actor, context) = match event {
            CollabEvent::Invocation {
                id,
                invocation,
                resource,
                action,
                argument,
                actor,
                context,
            } => (id, invocation, resource, action, argument, actor, context),
            CollabEvent::Handoff { request } => {
                self.handoff(&request);
                return true;
            }
            CollabEvent::RemoteAction { message } => {
                said_back(&self.spec.app, &message);
                return false;
            }
            CollabEvent::InvocationCancellation { id } => {
                // An invocation runs to its end in the frame it arrives in and is answered
                // there: by the time its cancellation arrives nothing of it is left to stop.
                tracing::debug!(id, "a cancellation of an invocation already answered");
                return false;
            }
            CollabEvent::TaskRun(_)
            | CollabEvent::TaskRunCancellation { .. }
            | CollabEvent::TaskRunControl { .. }
            | CollabEvent::WorkspaceControl { .. } => {
                tracing::warn!(
                    app = self.spec.app,
                    instance = self.spec.instance,
                    "a task or workspace request reached an app instance, which does not run them"
                );
                return false;
            }
            _ => return false,
        };
        let address = ActionAddress::new(resource, action);
        // The invoker is who the broker admitted, with the participant and the person it acts
        // for that the broker set in the delivery's context; nothing else is trusted.
        let invoker = ActionInvoker::delivered(&actor, context.as_deref());
        // A member's own record is that person's alone: anyone else is refused before anything
        // of it is read.
        let person = match invoker.kind {
            ActorKind::Person => Some(invoker.actor.as_str()),
            _ => invoker.on_behalf_of.as_deref(),
        };
        if !tilolang_tilo::reaches(self.record.route().record.as_deref(), person) {
            let update = failed(
                invocation,
                Refused::new("permission_denied", "this is another member's own record"),
            );
            self.answer_invocation(id, update);
            return true;
        }
        let attributed = context
            .as_deref()
            .map(|context| context.clone().with_invoker(invoker.clone()));
        let update = match self.invoke(&address, argument.as_deref(), invoker, attributed.as_ref())
        {
            Ok(output) => InvocationUpdate::Succeeded { invocation, output },
            Err(error) => failed(invocation, error),
        };
        self.answer_invocation(id, update);
        true
    }

    /// Bring the other apps' readings up to date: what their records took in is read again, and
    /// the readings the views asked for since are answered. A reading that moved runs the views
    /// that read it again on the next frame. Whether any did.
    fn read_used(&mut self) -> bool {
        if self.used.is_empty() {
            return false;
        }
        let Some(host) = self.instance.host() else {
            return false;
        };
        let runtime = host.runtime();
        if !self.asking {
            runtime.set_foreign_invoker(Doors(Rc::clone(&self.asks)));
            self.asking = true;
        }
        let mut moved = false;
        for used in &mut self.used {
            if std::mem::take(&mut used.moved) {
                used.app.host().sync_chunks(used.record.chunks());
                if let Some(first) = used.record.chunks().first() {
                    used.app.host().sync_from(first);
                }
                moved |= used.app.refresh(runtime);
            }
        }
        let mut apps: Vec<&mut UsedApp> = self.used.iter_mut().map(|used| &mut used.app).collect();
        moved |= tilolang_tilo::used::answer(runtime, &mut apps);
        if moved {
            (self.wake)();
        }
        moved
    }

    /// Send what the views and actions changed of the other apps' resources to those apps'
    /// doors, each as that app's action on its resource: a change comes back as its next reading.
    fn send_asks(&mut self) {
        let asks = std::mem::take(&mut *self.asks.borrow_mut());
        for invocation in asks {
            let Some((app, resource, action, argument)) = door_ask(&invocation) else {
                tracing::warn!(
                    app = self.spec.app,
                    ?invocation,
                    "a change of another app names no resource of it"
                );
                continue;
            };
            let request = tilo_actions::remote::InvokeRequest {
                invocation: InvocationId::new(),
                target: tilo_actions::remote::application_action_target(
                    &self.spec.workspace,
                    &app,
                    resource,
                    action,
                ),
                argument,
                context: None,
                confirmation: None,
                revision: None,
                on_behalf_of: None,
                wait_ms: None,
                appearance: None,
            };
            let message = tilo_actions::remote::RemoteActionMessage::Invoke(request);
            if let Err(error) = self
                .record
                .manager_mut()
                .try_send(CollabRequest::RemoteAction { message })
            {
                tracing::warn!(app = self.spec.app, to = app, %error, "a change of another app did not reach its door");
            }
        }
    }

    /// Hand the broker the outcome of the invocation `id`. An answer the connection does not
    /// take leaves its invoker waiting until the broker ends the invocation with the connection:
    /// that is said here.
    fn answer_invocation(&mut self, id: u64, update: InvocationUpdate) {
        if let Err(error) = self
            .record
            .manager_mut()
            .try_send(CollabRequest::UpdateInvocation { id, update })
        {
            tracing::error!(
                app = self.spec.app,
                instance = self.spec.instance,
                id,
                %error,
                "an invocation's outcome did not reach the broker"
            );
        }
    }

    /// Answer the shell's handoff step `request`: a checkpoint writes the record's on-disk copy
    /// and is done; a retirement is refused with why, as the tilolang host does not move a
    /// record to another workspace; an owner view is presentation the instance has no window
    /// of its own for, and wants no answer.
    fn handoff(&mut self, request: &collaboration::handoff::HandoffRequest) {
        use collaboration::handoff::{HandoffReport, HandoffRequest};
        let report = match request {
            HandoffRequest::Checkpoint { token } => {
                self.record.flush();
                HandoffReport::Done {
                    token: *token,
                    heads: Vec::new(),
                }
            }
            HandoffRequest::Retire { token, .. } => {
                tracing::warn!(
                    app = self.spec.app,
                    instance = self.spec.instance,
                    "the shell asked a tilolang instance to retire its record, which it cannot"
                );
                HandoffReport::Failed {
                    token: *token,
                    reason: "a tilolang app's instance does not move its record to another \
                             workspace"
                        .to_owned(),
                }
            }
            HandoffRequest::OwnerView { .. } => return,
        };
        if let Err(error) = self
            .record
            .manager_mut()
            .try_send(CollabRequest::ReportHandoff {
                message: report.encode(),
            })
        {
            tracing::error!(
                app = self.spec.app,
                instance = self.spec.instance,
                %error,
                "a handoff step's answer did not reach the shell"
            );
        }
    }

    /// Build the action `address` names for `invoker`, through the spec the instance publishes
    /// now, and run it: a record action is applied as the invoker's step; the host's own
    /// vocabulary is answered by the runtime, and what it commits is the invoker's.
    fn invoke(
        &mut self,
        address: &ActionAddress,
        argument: Option<&str>,
        invoker: ActionInvoker,
        ambient: Option<&ActionContext>,
    ) -> Result<tilo_actions::ActionOutput, Refused> {
        let surface = self
            .surface
            .borrow()
            .clone()
            .ok_or_else(|| Refused::new("action_unavailable", "the app has not started yet"))?;
        let shown = self.shown();
        let resources = surface.with_addressed(surface.resources(Some(&shown)), &address.resource);
        let by_person = invoker.kind == ActorKind::Person;
        let caller = caller_of(&invoker);
        CALLER.with(|slot| *slot.borrow_mut() = Some(caller.clone()));
        let encoded = encode(&resources, address, argument);
        CALLER.with(|slot| *slot.borrow_mut() = None);
        let encoded =
            encoded.map_err(|error| Refused::new(door_code(&error), error.to_string()))?;
        let host = self
            .instance
            .host()
            .ok_or_else(|| Refused::new("action_unavailable", "the app has not started yet"))?;
        if encoded.type_id == tilo_actions::resource::INVALID_ARGUMENTS_TYPE {
            return Err(Refused::new(
                "action_invalid_arguments",
                String::from_utf8_lossy(&encoded.payload),
            ));
        }
        if encoded.type_id == EDIT_TYPE {
            // A history verb, on the record as the invoker's own step.
            let edit: HistoryEdit = serde_json::from_slice(&encoded.payload)
                .map_err(|error| Refused::new("action_invalid_arguments", error.to_string()))?;
            let value = self
                .record
                .history(
                    &mut self.instance,
                    &edit,
                    &address.resource,
                    &self.resource.1,
                    invoker,
                    ambient,
                )
                .map_err(|error| Refused::new("action_failed", error.to_string()))?;
            return Ok(tilo_actions::ActionOutput {
                value,
                resources: Vec::new(),
            });
        }
        if encoded.type_id == VIEW_TYPE {
            return self
                .record
                .view_history(&encoded.payload)
                .map_err(|error| Refused::new("action_failed", error));
        }
        if encoded.type_id == tilo_resource::read::TYPE {
            return self
                .read(&encoded.payload)
                .map_err(|error| Refused::new("action_failed", error));
        }
        if encoded.type_id == tilo_actions::remote::APPLICATION_ACTIVATE_TYPE {
            self.instance.activate_window();
            return Ok(tilo_actions::remote::activation_requested());
        }
        if encoded.type_id == ACTION_TYPE {
            let action: DynAction = serde_json::from_slice(&encoded.payload)
                .map_err(|error| Refused::new("action_invalid_arguments", error.to_string()))?;
            self.record
                .apply_invoked(&mut self.instance, &action, &invoker, ambient)
                .map_err(|error| Refused::new("action_failed", error.to_string()))?;
            return Ok(returned(&host, &action));
        }
        if address.resource == HOST_RESOURCE
            && encoded.type_id == <HostEffect as tilo_resource::Effect>::OPERATION
        {
            let effect = <HostEffect as tilo_resource::Effect>::from_payload(&encoded.payload)
                .map_err(|error| Refused::new("action_invalid_arguments", error.to_string()))?;
            let first = host.next_ticket();
            let output = host
                .answer(effect, Some(&caller))
                .map_err(|error| Refused::new("action_failed", error))?;
            for committed in host.take_committed_from(first) {
                self.record
                    .apply_invoked(&mut self.instance, &committed.action, &invoker, ambient)
                    .map_err(|error| Refused::new("action_failed", error.to_string()))?;
            }
            return Ok(output);
        }
        if encoded.type_id == <OpenEffect as tilo_resource::Effect>::OPERATION {
            // A search hit, or an agent's `open`: the window shows what it names.
            let open = <OpenEffect as tilo_resource::Effect>::from_payload(&encoded.payload)
                .map_err(|error| Refused::new("action_invalid_arguments", error.to_string()))?;
            self.instance.open_requests().borrow_mut().push(open);
            // A person who chose it comes to see it: the window showing it comes forward. An
            // agent's `open` leaves the person where they are.
            if by_person {
                self.instance.activate_window();
            }
            return Ok(tilo_actions::ActionOutput::default());
        }
        Err(Refused::new(
            "action_unknown",
            format!("the tilolang host does not run `{}`", encoded.type_id),
        ))
    }

    /// Answer `read` (`tilo_resource::read`): what the record holds, through the runtime, now or
    /// at a past version.
    fn read(&self, payload: &[u8]) -> Result<tilo_actions::ActionOutput, String> {
        let args: serde_json::Value = if payload.is_empty() {
            serde_json::Value::Null
        } else {
            tilo_actions::decode_json(payload).map_err(|error| error.to_string())?
        };
        let item = args.get("item").and_then(serde_json::Value::as_str);
        let value = if let Some(version) = args.get("version").and_then(serde_json::Value::as_str) {
            let doc = self.record.doc();
            let heads = collaboration::history_source::steps::resolve(doc, version)?;
            let frozen = doc
                .clone()
                .fork_at(&heads)
                .map_err(|error| error.to_string())?;
            // A runtime of its own over the frozen record, as a framework app reads a past
            // version; a read runs no action.
            let past = Host::new(
                &frozen,
                tilolang_tilo::runtime::Principal::new("reader"),
                tilolang_tilo::authority::holding_nothing("reader"),
            )
            .map_err(|error| error.to_string())?;
            past.load_bundle(&self.bundle)
                .map_err(|error| error.to_string())?;
            verbs::review::bounded(serde_json::json!({
                "version": heads.iter().map(ToString::to_string).collect::<Vec<_>>().join("+"),
                "heads": heads.iter().map(ToString::to_string).collect::<Vec<_>>(),
                "content": past.read_content(item)?,
            }))?
        } else {
            let host = self
                .instance
                .host()
                .ok_or_else(|| "the app has not started yet".to_owned())?;
            sync_record(&host, &self.record);
            host.read_content(item)?
        };
        Ok(tilo_actions::ActionOutput {
            value: Some(value),
            resources: Vec::new(),
        })
    }

    /// The resources the window shows, as published ids, with the resources that hold them: a
    /// row's actions are published on its own resource, and a resource publishes with what
    /// holds it (the record's tree). What the surface a window reads is bounded by.
    fn shown(&self) -> Vec<String> {
        let mut out = std::collections::BTreeSet::new();
        for id in self.instance.shown_resources() {
            let mut held = id.as_str();
            out.insert(id.clone());
            while let Some((parent, _)) = held.rsplit_once('/') {
                if !parent.contains(':') {
                    break;
                }
                out.insert(parent.to_owned());
                held = parent;
            }
        }
        out.into_iter().collect()
    }

    /// Publish what the instance offers, when it changed since the broker last took it.
    fn publish(&mut self) {
        let Some(surface) = self.surface.borrow().clone() else {
            return;
        };
        // Nothing the surface is built from changed since the broker took it: a keystroke into
        // a field nothing published reads, and every turn that changed nothing, cost no more
        // with ten thousand records or items than with one.
        let shown = self.shown();
        let inputs = surface.inputs(&shown);
        let whole = (inputs.0, inputs.1.clone(), inputs.2);
        if self.directory_from.as_ref() != Some(&whole) {
            self.directory_from = Some(whole);
            self.directory_behind = true;
        }
        if self
            .published_from
            .as_ref()
            .is_some_and(|(from, at)| Rc::ptr_eq(from, &surface) && *at == inputs)
        {
            return;
        }
        if self.relabel(&surface, inputs.clone()) {
            return;
        }
        let _phase = tilo_resource::profiling::Phase::start("tl.host.publish");
        let resources = surface.resources(Some(&shown));
        let handlers: HashSet<String> = HashSet::from([
            <HostEffect as tilo_resource::Effect>::OPERATION.to_owned(),
            <OpenEffect as tilo_resource::Effect>::OPERATION.to_owned(),
            VIEW_TYPE.to_owned(),
            tilo_resource::read::TYPE.to_owned(),
            tilo_actions::remote::APPLICATION_ACTIVATE_TYPE.to_owned(),
        ]);
        let violations = tilo_actions::validate_surface(&resources, &self.registry, &handlers);
        if !violations.is_empty() {
            for violation in &violations {
                tracing::error!(
                    address = %violation.address,
                    reason = %violation.reason,
                    "refusing to publish an invalid resource/action surface"
                );
            }
            // A stale catalogue that looks valid is worse than none.
            self.directory.withdraw();
            self.directory_behind = false;
            self.directory_from = None;
            self.offer(&[]);
            if self
                .published
                .as_ref()
                .is_some_and(|(held, _)| !held.is_empty())
            {
                let (delta, state) = SurfacePublication::next(
                    self.published.as_ref().map(|(d, s)| (s, d.as_slice())),
                    &[],
                );
                if self
                    .record
                    .manager_mut()
                    .send(CollabRequest::PublishActionSurface { delta })
                {
                    self.published = Some((Vec::new(), state));
                    self.published_from = None;
                }
            }
            self.refused = violations;
            return;
        }
        self.refused.clear();
        let mut descriptors = tilo_actions::door::survey(&resources);
        self.answer_what_the_window_shows(&mut descriptors);
        self.offer(&descriptors);
        // The directory is built when a caller asks for it: what the surface holds is the
        // snapshot's input, not the answer to a question nobody asked.
        if self
            .published
            .as_ref()
            .is_some_and(|(held, _)| *held == descriptors)
        {
            self.published_from = Some((surface, inputs));
            return;
        }
        let prior = self
            .published
            .as_ref()
            .map(|(held, state)| (state, held.as_slice()));
        let (delta, state) = SurfacePublication::next(prior, &descriptors);
        // A surface the connection did not take is offered again on the next frame.
        if delta.is_empty()
            || self
                .record
                .manager_mut()
                .send(CollabRequest::PublishActionSurface { delta })
        {
            self.published = Some((descriptors, state));
            self.published_from = Some((surface, inputs));
        }
    }

    /// Offer the window the person's command palette `descriptors`, the actions the instance
    /// publishes on its connection: the window offers them the way it offers its own, with the
    /// labels, keys and icons the app gives them. Nothing is sent while the broker's copy is
    /// what the window holds.
    fn offer(&mut self, descriptors: &[ActionDescriptor]) {
        let unchanged = self
            .published
            .as_ref()
            .is_some_and(|(held, _)| held.as_slice() == descriptors);
        if !unchanged {
            self.say_actions(ActionCatalogue::All, descriptors);
        }
    }

    /// Tell the window `catalogue` of `descriptors`, as the action directory carries them.
    fn say_actions(
        &mut self,
        catalogue: fn(Vec<u8>) -> ActionCatalogue,
        descriptors: &[ActionDescriptor],
    ) {
        match serde_json::to_vec(descriptors) {
            Ok(actions) => {
                self.instance.say(ToUi::Actions(catalogue(actions)));
                // The window takes it now, not on the frame after.
                self.instance.flush();
            }
            Err(error) => tracing::error!(%error, "an action catalogue did not encode"),
        }
    }

    /// Run the next action the person chose at the window as their own step: the same door an
    /// invocation the broker delivered for them takes, so the action runs for them however it
    /// reached the app. One a frame, so what a press opened is sent, and the window hears it
    /// ran, before the press after it ([`tilolang_tilo::instance::Instance::show_ran`]).
    /// Whether there was one.
    fn chosen(&mut self) -> bool {
        let Some((press, address, argument)) = self.instance.take_chosen() else {
            return false;
        };
        let invoker = ActionInvoker::actor(self.spec.person.clone());
        let opens_what_it_made = self.is_primary(&address);
        match self.invoke(&address, argument.as_deref(), invoker, None) {
            Ok(output) => {
                // Choosing what a press of a primary action does asks for the same: what it
                // made opens beside the list.
                if opens_what_it_made {
                    for anchor in output.resources {
                        self.instance.opens_what_it_made(anchor);
                    }
                }
            }
            Err(refused) => self.instance.action_refused(refused.message),
        }
        // The press ran: once it is in the record this frame sends what it opened, then tells
        // the window it ran, so what the person typed after it goes there.
        self.instance.press_ran(press);
        true
    }

    /// Whether the app declares the action `address` names `primary`: what it made opens when
    /// a person runs it, whether from a press or from the palette.
    fn is_primary(&self, address: &ActionAddress) -> bool {
        self.instance
            .host()
            .and_then(|host| {
                host.find_in_packages(|package| {
                    package
                        .meta
                        .actions
                        .iter()
                        .find(|action| {
                            action.id == address.action
                                && tilolang_tilo::surface::is_primary(action)
                        })
                        .map(|_| ())
                })
            })
            .is_some()
    }

    /// Publish a change to the surface that moved only labels (a note's title typed into) as
    /// those resources alone: their descriptors patched, and a delta of them, at the cost of
    /// what changed whatever the notebook. `inputs` are the surface's ([`Surface::inputs`]).
    /// Whether this turn's publishing is done; when the surface changed in more than labels
    /// since the broker took it, the caller builds it whole.
    ///
    /// Nothing is validated again: no rule reads a label. A change no label moved with is done
    /// here too when it read nothing an action offered on the window's rows reads either (a note's
    /// body typed into): otherwise the caller reads what the app says of those actions again,
    /// since that is not a label and the change may have moved it (a task checked off, and
    /// `Clear done` can run now).
    fn relabel(&mut self, surface: &Rc<Surface>, inputs: SurfaceInputs) -> bool {
        let Some((from, taken)) = &self.published_from else {
            return false;
        };
        if !Rc::ptr_eq(from, surface) || taken.1 != inputs.1 || taken.3 != inputs.3 {
            return false;
        }
        let Some(labels) = surface.publications.borrow().labels_since(taken.0) else {
            return false;
        };
        // What the app says of an action on a resource the surface holds — what it is called
        // there, the icon beside it and whether it can run — is read again when a change reached
        // it: a change the surface shows nothing of (a task checked off, and `Clear done` can run
        // now) moves that and no label.
        let answered = self.forget_answers();
        if labels.is_empty() {
            if answered {
                return false;
            }
            self.published_from = Some((Rc::clone(surface), inputs));
            return true;
        }
        if answered {
            return false;
        }
        let Some((descriptors, state)) = &mut self.published else {
            return false;
        };
        if labels
            .iter()
            .any(|(resource, _)| state.indices(resource).is_none())
        {
            return false;
        }
        let _phase = tilo_resource::profiling::Phase::start("tl.host.relabel");
        for (resource, label) in &labels {
            for index in state.indices(resource).unwrap_or_default() {
                if let Some(descriptor) = descriptors.get_mut(*index) {
                    descriptor.resource_label.clone_from(label);
                }
            }
        }
        self.directory_behind = true;
        let changed: Vec<&str> = labels
            .iter()
            .map(|(resource, _)| resource.as_str())
            .collect();
        let Some(delta) = state.relabel(descriptors, &changed) else {
            return false;
        };
        // The window's palette shows the labels the person types into, which are what moved.
        let relabeled: Vec<tilo_actions::ActionDescriptor> = descriptors
            .iter()
            .filter(|descriptor| changed.contains(&descriptor.address.resource.as_str()))
            .cloned()
            .collect();
        self.say_actions(ActionCatalogue::Relabeled, &relabeled);
        if !delta.is_empty()
            && !self
                .record
                .manager_mut()
                .send(CollabRequest::PublishActionSurface { delta })
        {
            // What the broker holds is no longer what was patched: a later turn sends the
            // surface whole, as a reset.
            self.published = None;
            self.published_from = None;
            return true;
        }
        self.published_from = Some((Rc::clone(surface), inputs));
        true
    }

    /// Write what the record holds to its on-disk copy, as the instance ends.
    pub fn flush(&mut self) {
        self.record.flush();
    }
}

/// Another app the app uses, read from its own record.
struct Used {
    record: ReadRecord,
    app: UsedApp,
    /// Whether the record took in a change its readings have not been read again for.
    moved: bool,
}

/// The other apps `bundle`'s app uses ([`Spec::installed`]), each joined on `record`'s connection
/// and read through its own package.
fn used_apps(spec: &Spec, bundle: &Bundle, record: &mut InstanceRecord) -> Vec<Used> {
    let workspace = record.route().cluster_id;
    bundle
        .imports
        .iter()
        .filter_map(|import| {
            let records = spec.installed.get(&import.meta.app)?;
            let packages = packages_of(&bundle.imports, &import.meta);
            let app = UsedApp::new(&packages, &spec.person)
                .map_err(|error| {
                    tracing::error!(app = spec.app, used = import.meta.app, %error, "a used app's package did not load");
                })
                .ok()?;
            let route = crate::route_under(&import.meta, records, workspace, &spec.person);
            Some(Used {
                record: ReadRecord::join(route, record.manager_mut()),
                app,
                moved: false,
            })
        })
        .collect()
}

/// The packages `meta`'s app is read through, from `imports`: those it uses, in the order they
/// load, then its own.
fn packages_of<'a>(
    imports: &'a [tilolang_tilo::bundle::Import],
    meta: &'a tilolang_tilo::runtime::Meta,
) -> Vec<(&'a [u8], &'a tilolang_tilo::runtime::Meta)> {
    let mut needed: HashSet<&str> = HashSet::new();
    let mut pending = vec![meta];
    while let Some(next) = pending.pop() {
        for used in &next.uses {
            if needed.insert(used.as_str())
                && let Some(import) = imports.iter().find(|import| import.meta.app == *used)
            {
                pending.push(&import.meta);
            }
        }
    }
    imports
        .iter()
        .filter(|import| import.meta.app != meta.app && needed.contains(import.meta.app.as_str()))
        .map(|import| (import.wasm.as_slice(), &import.meta))
        .chain(std::iter::once((
            imports
                .iter()
                .find(|import| import.meta.app == meta.app)
                .map_or(&[][..], |import| import.wasm.as_slice()),
            meta,
        )))
        .collect()
}

/// Where the runtime sends what a view or an action changes of another app's resources: queued
/// for [`Hosted::send_asks`], and done here; the change comes back as the resource's next reading.
struct Doors(Rc<RefCell<Vec<tilolang_tilo::runtime::delegate::Invocation>>>);

impl tilolang_tilo::runtime::delegate::Invoker for Doors {
    fn invoke(
        &self,
        invocation: &tilolang_tilo::runtime::delegate::Invocation,
    ) -> tilolang_tilo::runtime::delegate::Invoked {
        self.0.borrow_mut().push(invocation.clone());
        tilolang_tilo::runtime::delegate::Invoked::Sent
    }
}

/// `invocation` as its app's door takes it: the app, the resource, the action and its
/// arguments. An action is invoked on its subject, which its arguments leave out; an edit of a
/// field is the resource's own field edit. `None` for an action that names no resource.
fn door_ask(
    invocation: &tilolang_tilo::runtime::delegate::Invocation,
) -> Option<(String, String, String, tilo_actions::remote::InvokeArgument)> {
    use tilolang_tilo::runtime::Value;
    use tilolang_tilo::runtime::delegate::Invocation;
    match invocation {
        Invocation::Action {
            app,
            action,
            subject: Some(subject),
            from,
            ..
        } => {
            let mut arguments = invocation.door_arguments();
            if let Some(from) = from {
                arguments.insert(
                    tilo_actions::params::FROM_ARGUMENT.to_owned(),
                    tilo_actions::params::MadeFrom {
                        resource: from.id_string(),
                        kind: from.ty.clone(),
                    }
                    .to_json(),
                );
            }
            Some((
                app.clone(),
                subject.id_string(),
                action.clone(),
                tilo_actions::remote::InvokeArgument::Json(arguments),
            ))
        }
        Invocation::Action { .. } => None,
        Invocation::Edit {
            app,
            target,
            field_id,
            value,
            ..
        } => {
            let action = match value {
                Value::Bool(_) => tilolang_tilo::action::EDIT_FLAG_ACTION,
                _ => tilolang_tilo::action::EDIT_TEXT_ACTION,
            };
            let value = tilolang_tilo::runtime::value::to_argument(value).unwrap_or_default();
            let arguments = serde_json::Map::from_iter([
                ("field".to_owned(), field_id.clone().into()),
                ("value".to_owned(), value),
            ]);
            Some((
                app.clone(),
                target.id_string(),
                action.to_owned(),
                tilo_actions::remote::InvokeArgument::Json(arguments),
            ))
        }
    }
}

/// Say what another app's door answered of a change asked of it, when it refused or failed:
/// the change never came, and its resource reads as it did.
fn said_back(app: &str, message: &tilo_actions::remote::RemoteActionMessage) {
    use tilo_actions::remote::{ReceiptStatus, RemoteActionMessage};
    if let RemoteActionMessage::Receipt(receipt) = message {
        match &receipt.status {
            ReceiptStatus::Rejected { error } => {
                tracing::warn!(
                    app,
                    ?error,
                    "another app's door refused a change asked of it"
                );
            }
            ReceiptStatus::Failed { error } => {
                tracing::warn!(app, ?error, "a change asked of another app's door failed");
            }
            _ => {}
        }
    }
}

/// Why an invocation was refused, as the broker reports it to its invoker.
#[derive(Debug)]
struct Refused {
    code: String,
    message: String,
}

impl Refused {
    fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

fn failed(invocation: InvocationId, refused: Refused) -> InvocationUpdate {
    InvocationUpdate::Failed {
        invocation,
        error: tilo_actions::ActionFailure {
            code: refused.code,
            message: refused.message,
            details: None,
            retryable: false,
        },
    }
}

/// The action `address` names, built through its published spec from `argument`: typed JSON
/// arguments, or the legacy text of a prompted action.
fn encode(
    resources: &[tilo_actions::ResourceSpec],
    address: &ActionAddress,
    argument: Option<&str>,
) -> Result<EncodedAction, tilo_actions::DoorError> {
    match argument.and_then(|text| text.strip_prefix(tilo_actions::ARGUMENTS_JSON_PREFIX)) {
        Some(json) => {
            let arguments: serde_json::Map<String, serde_json::Value> = serde_json::from_str(json)
                .map_err(|error| tilo_actions::DoorError::InvalidArguments(error.to_string()))?;
            tilo_actions::door::encode_json(resources, address, &arguments)
        }
        None => tilo_actions::door::encode(resources, address, argument),
    }
}

fn door_code(error: &tilo_actions::DoorError) -> &'static str {
    match error {
        tilo_actions::DoorError::Unknown(_) => "action_unknown",
        tilo_actions::DoorError::Unavailable { .. } => "action_unavailable",
        _ => "action_invalid_arguments",
    }
}

/// What the action `action` answers: the resource it returned, by its published id, which a
/// task's program goes on with.
fn returned(host: &tilolang_tilo::host::Host, action: &DynAction) -> tilo_actions::ActionOutput {
    let returned = host
        .take_returned(&action.action_id)
        .map(|made| tilolang_tilo::publish::resource_id(&action.app, &made.id_string()));
    tilo_actions::ActionOutput {
        value: None,
        resources: returned.into_iter().collect(),
    }
}

/// What an [`Applier`] has done with its change so far.
enum Applying {
    /// The change has not arrived on its connection yet.
    Awaiting(Vec<u8>),
    /// It arrived; it runs once what the record publishes is built.
    Arrived(Box<runtime_portal::tilolang_host::Change>),
    /// It ran: its answer, sent once the workspace was handed what it wrote.
    Ran(serde_json::Value),
    /// The answer was sent.
    Answered,
}

/// The origin of one record applying one change: an agent task's invocation the broker
/// admitted (`docs/designs/app-language/agents.md`, "Reading records outside their app"). No
/// window and no instance: the record on its connection, which holds the app's custody as an
/// instance's does, and the app's package in a runtime of its own. The action's code runs
/// against the record as it is when the change is applied, so its requirements are decided on
/// what others wrote since the task read it; what it wrote is applied as the invoker's step
/// and handed to the workspace, and the answer says how it differed from the task's preview.
pub struct Applier {
    spec: Spec,
    record: InstanceRecord,
    host: Host,
    surface: Rc<Surface>,
    /// The app's resource, by its id and kind: where its history verbs are published.
    resource: (String, String),
    connection: std::os::unix::net::UnixStream,
    applying: Applying,
    /// Whether the answer carries the whole record: the root this change needed was made for
    /// it.
    whole: bool,
}

impl Applier {
    /// The origin of `record` for `bundle`'s app, taking its change on `connection`.
    pub fn open(
        spec: Spec,
        record: InstanceRecord,
        bundle: &Bundle,
        phrases: &Phrases,
        wake: Waker,
        connection: std::os::fd::OwnedFd,
    ) -> Result<Self, String> {
        let host = Host::new(
            record.doc(),
            tilolang_tilo::runtime::Principal::new(spec.person.clone()),
            || None,
        )
        .map_err(|error| error.to_string())?;
        host.set_clock(tilolang_tilo::host::system_time);
        host.load_bundle(bundle)
            .map_err(|error| error.to_string())?;
        // A change made to the app's record at its origin reads in the language the app's window
        // would read in: the person's, not this host process's.
        let phrases = window_phrases(phrases, &spec, bundle);
        let activate = tilo_actions::remote::activate_spec(
            phrases.say("app-activate-window", &[]),
            phrases.say("app-activate-window-description", &[]),
        );
        let surface = Rc::new(Surface::new(&bundle.meta.app, activate, &host, wake));
        let connection = std::os::unix::net::UnixStream::from(connection);
        connection
            .set_nonblocking(true)
            .map_err(|error| error.to_string())?;
        Ok(Self {
            resource: (
                package_resource_id(&bundle.meta.app),
                bundle.meta.package_kind(),
            ),
            spec,
            record,
            host,
            surface,
            connection,
            applying: Applying::Awaiting(Vec::new()),
            whole: false,
        })
    }

    /// What the applier is.
    #[must_use]
    pub fn spec(&self) -> &Spec {
        &self.spec
    }

    /// The record it changes, as it holds it.
    #[must_use]
    pub fn record(&self) -> &InstanceRecord {
        &self.record
    }

    /// The change's connection, to wait on.
    pub fn connection(&self) -> std::os::fd::BorrowedFd<'_> {
        use std::os::fd::AsFd as _;
        self.connection.as_fd()
    }

    /// Whether the change was answered: the applier has nothing left to do.
    #[must_use]
    pub fn answered(&self) -> bool {
        matches!(self.applying, Applying::Answered)
    }

    /// One step: take in what the workspace sent and what came on the connection, run the change
    /// once it can, and answer once the workspace was handed what it wrote. Whether anything changed.
    pub fn turn(&mut self) -> bool {
        let mut changed = self.record.take_in_record();
        if changed {
            sync_record(&self.host, &self.record);
        }
        self.surface.update();
        let state = std::mem::replace(&mut self.applying, Applying::Answered);
        self.applying = match state {
            Applying::Awaiting(mut read) => match self.read_change(&mut read) {
                Ok(Some(change)) => {
                    changed = true;
                    Applying::Arrived(Box::new(change))
                }
                Ok(None) => Applying::Awaiting(read),
                Err(why) => {
                    changed = true;
                    Applying::Ran(serde_json::json!({ "failed": why }))
                }
            },
            Applying::Arrived(change) if self.surface.publications.borrow().is_building() => {
                Applying::Arrived(change)
            }
            Applying::Arrived(change) => {
                changed = true;
                match self.make_root(&change) {
                    // The root it made publishes first; the change resolves against it then.
                    Ok(true) => Applying::Arrived(change),
                    Ok(false) => Applying::Ran(self.apply(&change)),
                    Err(why) => Applying::Ran(serde_json::json!({ "failed": why })),
                }
            }
            other => other,
        };
        if let Applying::Ran(answer) = &self.applying
            && (self.record.share().is_some() || !self.record.unsent())
        {
            use std::io::Write as _;
            let mut line = answer.to_string().into_bytes();
            line.push(b'\n');
            if let Err(error) = (&self.connection).write_all(&line) {
                tracing::warn!(app = self.spec.app, %error, "a change's answer did not reach the session");
            }
            self.applying = Applying::Answered;
            changed = true;
        }
        changed
    }

    /// The change, once its whole line arrived on the connection.
    fn read_change(
        &self,
        read: &mut Vec<u8>,
    ) -> Result<Option<runtime_portal::tilolang_host::Change>, String> {
        use std::io::Read as _;
        let mut chunk = [0_u8; 4096];
        loop {
            match (&self.connection).read(&mut chunk) {
                Ok(0) => return Err("the session left before it said the change".to_owned()),
                Ok(n) => read.extend_from_slice(chunk.get(..n).unwrap_or_default()),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(error) => return Err(format!("the change's connection failed: {error}")),
            }
        }
        let Some(end) = read.iter().position(|byte| *byte == b'\n') else {
            return Ok(None);
        };
        let line = read.get(..end).unwrap_or_default();
        serde_json::from_slice(line)
            .map(Some)
            .map_err(|error| format!("the session said a change that does not decode: {error}"))
    }

    /// Make the app's root when `change` addresses it and the record does not hold it yet: the
    /// one per-workspace resource of each of the package's root types is made the first time it
    /// is wanted, here as in a window that opens it, and a task reads it from the package
    /// whether or not any window ever made it. A package may declare several roots
    /// (`root viewing: Viewing`, `root Desk`); the change names which. It is applied as the
    /// change's invoker's step, before the change. Whether it made it.
    fn make_root(
        &mut self,
        change: &runtime_portal::tilolang_host::Change,
    ) -> Result<bool, String> {
        let prefix = tilolang_tilo::publish::resource_id(&self.spec.app, "");
        // The resource's record: an item's path starts with it.
        let Some(doc) = change
            .resource
            .strip_prefix(&prefix)
            .and_then(|path| path.split('/').next())
        else {
            return Ok(false);
        };
        let Some(ty) = doc.strip_prefix("root-") else {
            return Ok(false);
        };
        if self.host.store().contains(doc) {
            return Ok(false);
        }
        let declared = self
            .host
            .package(&self.spec.app)
            .is_some_and(|package| package.declares_root(ty));
        if !declared {
            return Err(format!(
                "{} is no root of {}",
                change.resource, self.spec.app
            ));
        }
        self.host
            .root_record(&self.spec.app, ty)
            .map_err(|error| error.to_string())?;
        let invoker: ActionInvoker = serde_json::from_value(change.invoker.clone())
            .map_err(|error| format!("the change's invoker does not decode: {error}"))?;
        let ambient: Option<ActionContext> = change
            .context
            .clone()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|error| format!("the change's context does not decode: {error}"))?;
        let ambient = ambient.map(|context| context.with_invoker(invoker.clone()));
        // A record without its root is one the task read as none, or as what an app's window
        // never shared: the answer carries the whole record, which is small then.
        self.whole = true;
        for committed in self.host.take_committed() {
            self.record
                .apply_change(&committed.action, &invoker, ambient.as_ref())
                .map_err(|error| error.to_string())?;
        }
        sync_record(&self.host, &self.record);
        self.surface.update();
        Ok(true)
    }

    /// Run `change` against the record as it is now and apply what it wrote: its answer.
    fn apply(&mut self, change: &runtime_portal::tilolang_host::Change) -> serde_json::Value {
        let invoker: ActionInvoker = match serde_json::from_value(change.invoker.clone()) {
            Ok(invoker) => invoker,
            Err(error) => {
                return serde_json::json!({"failed": format!("the change's invoker does not decode: {error}")});
            }
        };
        let ambient: Option<ActionContext> = match change
            .context
            .clone()
            .map(serde_json::from_value)
            .transpose()
        {
            Ok(ambient) => ambient,
            Err(error) => {
                return serde_json::json!({"failed": format!("the change's context does not decode: {error}")});
            }
        };
        // The record as the workspace holds it now: requirements are decided on what others wrote.
        sync_record(&self.host, &self.record);
        let address = ActionAddress::new(change.resource.clone(), change.action.clone());
        // An agent asks of the resource it names, whatever a window shows: the workspace is not
        // published for one change, and an applier shows nothing.
        let resources = self.surface.addressed(&[], &change.resource);
        let caller = caller_of(&invoker);
        CALLER.with(|slot| *slot.borrow_mut() = Some(caller));
        let encoded = tilo_actions::door::encode_json(&resources, &address, &change.arguments);
        CALLER.with(|slot| *slot.borrow_mut() = None);
        let encoded = match encoded {
            Ok(encoded) if encoded.type_id == tilo_actions::resource::INVALID_ARGUMENTS_TYPE => {
                return serde_json::json!({"failed": String::from_utf8_lossy(&encoded.payload)});
            }
            Ok(encoded) => encoded,
            Err(error) => {
                tracing::warn!(
                    app = self.spec.app,
                    resource = address.resource,
                    action = address.action,
                    offered = resources.len(),
                    %error,
                    "the record's origin does not offer the change it was handed"
                );
                return serde_json::json!({"failed": error.to_string()});
            }
        };
        let ambient = ambient.map(|context| context.with_invoker(invoker.clone()));
        if encoded.type_id == EDIT_TYPE {
            let edit: HistoryEdit = match serde_json::from_slice(&encoded.payload) {
                Ok(edit) => edit,
                Err(error) => return serde_json::json!({"failed": error.to_string()}),
            };
            let before = self.record.marks();
            return match self.record.history_at_origin(
                &edit,
                &address.resource,
                &self.resource.1,
                invoker,
                ambient.as_ref(),
            ) {
                Ok(value) => {
                    sync_record(&self.host, &self.record);
                    // What it wrote, for the task to read back at once, as a change's answer.
                    let wrote = self.wrote_since(&before);
                    serde_json::json!({"ok": {"value": value, "resources": []}, "change": wrote})
                }
                Err(error) => serde_json::json!({"failed": error.to_string()}),
            };
        }
        if encoded.type_id != ACTION_TYPE {
            return serde_json::json!({"failed": format!(
                "{} is no change of the record",
                address.action
            )});
        }
        let action: DynAction = match serde_json::from_slice(&encoded.payload) {
            Ok(action) => action,
            Err(error) => return serde_json::json!({"failed": error.to_string()}),
        };
        // What it wrote includes the root it made for it, which the task's read lacks.
        let before = if std::mem::take(&mut self.whole) {
            tilolang_tilo::chunk::Stamps::new()
        } else {
            self.record.marks()
        };
        if let Err(error) = self
            .record
            .apply_change(&action, &invoker, ambient.as_ref())
        {
            return serde_json::json!({"failed": error.to_string()});
        }
        sync_record(&self.host, &self.record);
        let output = returned(&self.host, &action);
        let differs = change.preview.as_ref().is_some_and(|preview| {
            serde_json::to_value(&action).is_ok_and(|applied| applied != *preview)
        });
        // What it wrote, for the task to read back at once: the workspace takes it in its own time.
        let wrote = self.wrote_since(&before);
        serde_json::json!({
            "ok": {"value": output.value, "resources": output.resources},
            "differs": differs,
            "change": wrote,
        })
    }

    /// What the record wrote since it held `before`, as the change a caller reads back.
    fn wrote_since(&self, before: &tilolang_tilo::chunk::Stamps) -> String {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD.encode(self.record.changes_since(before))
    }
}
