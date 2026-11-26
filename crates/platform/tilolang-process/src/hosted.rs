//! One app instance in a tilolang host: the app's runtime over the workspace's store, on the
//! broker connection the session opened for that app's launch, driving its window with component
//! patches.
//!
//! What the window's own process did before (the framework's root) is the instance's here:
//!
//! - **The store:** every resource the app keeps is in the workspace's store, read and written
//!   over the instance's connection ([`crate::connection`]). What an action writes is sent as
//!   one change of whoever invoked it, under its invocation.
//! - **Actions:** the instance publishes the app's resources and actions on its connection, as
//!   the toolkit publishes an app's ([`tilo_actions::api::survey`], deltas through
//!   [`SurfacePublication`]), and runs what the broker delivers: an action the broker admitted for
//!   its invoker is built through the published spec and applied as that invoker's step. It
//!   offers an action directory on the connection too, answering it from what it published
//!   ([`PublishedDirectory`]): what makes the instance one the API and agents can browse.
//! - **What every app publishes:** on its app's resource the instance publishes the history
//!   verbs (Undo, Redo, History, ...: `collaboration::history_source::verbs`), `read`
//!   (`tilo_resource::read`) and, where it shows a window, "Show this window"
//!   (`tilo_actions::remote::activate_spec`), and answers them as a framework app does: undo and
//!   redo through the store, `read` through the runtime, and "Show this window" by asking its
//!   window to come forward. The person's Undo on a notice in the window is the same `undo`, as
//!   theirs: there is one undo.
//! - **The window:** a window's instance sends it patches and hears what the person did over the
//!   patch connection; an instance that shows nothing ([`Spec::shows`]) sends its patches nowhere
//!   and hears nothing.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashSet};
use std::fmt::Write as _;
use std::rc::Rc;
use std::sync::Arc;

use crate::connection::Connection;
use crate::personal::PersonalDocument;
use collaboration::action_directory::PublishedDirectory;
use collaboration::history_source::verbs::{EDIT_TYPE, HistoryEdit, VIEW_TYPE};
use collaboration::{CollabEvent, CollabRequest, SurfaceDelta, SurfacePublication};
use runtime_portal::process_link::Shows;
use tilo_actions::{
    ActionAddress, ActionContext, ActionDescriptor, ActionInvoker, EncodedAction, InvocationUpdate,
    ResourceSpec,
};
use tilo_render_protocol::WireError;
use tilo_render_protocol::patch::{ActionCatalogue, ToUi, Transport};
use tilolang_tilo::action::{ACTION_TYPE, DynAction};
use tilolang_tilo::background::Waker;
use tilolang_tilo::bundle::Bundle;
use tilolang_tilo::host::Host;
use tilolang_tilo::instance::{Config, Instance, LaterBuilds, Mode, Platform, Publication};
use tilolang_tilo::publish::HOST_RESOURCE;
use tilolang_tilo::publish::{Caller, Moved, Planner, Publications, publish_package};
use tilolang_tilo::resources::ResourceStore;
use tilolang_tilo::runtime::ids::MemberId;
use tilolang_tilo::runtime::{InvokeOutcome, Value};
use tilolang_tilo::used::UsedApp;
use tilolang_tilo::{HostEffect, OpenEffect};

mod used;
use used::{Used, used_apps};

use crate::phrases::Phrases;
use crate::presence::Presence;

thread_local! {
    /// The caller of the invocation being built on this thread, as the broker delivered it.
    static CALLER: RefCell<Option<Caller>> = const { RefCell::new(None) };
}

/// The person's date and time now: the device's clock, which is the one their window reads, so a
/// day an instance writes and a day their window heads a row by are the same day.
fn local_now() -> jiff::Zoned {
    tilo_clock::local::now()
}

/// What one instance is: the app, the workspace, the window and who is at it, as the session named
/// them for the app's launch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Spec {
    pub app: String,
    pub workspace: String,
    /// The window's instance.
    pub instance: u64,
    /// The member at the window.
    pub person: String,
    /// The tilolang apps installed here, by id, each with the id its documents are kept under: a
    /// package the app uses that is one of them is another app, read from its own document.
    pub installed: BTreeMap<String, String>,
    /// The person's language (`de-DE`), the one their windows read in, as the session resolved it
    /// (`InstanceRequest::language`): the window speaks it where the package ships it, and the
    /// package's source language otherwise. Empty for a session that names none, whose host reads
    /// its own process's language.
    pub language: String,
    /// What the person may do in the app as the session handed it with the launch
    /// (`InstanceRequest::access`): the views offer what it denies them disabled, and the
    /// runtime refuses it before it reaches the broker. `None` where the session holds no policy
    /// for the workspace: the broker alone decides.
    pub access: Option<tilo_action_policy::MemberAccess>,
    /// Whether the instance shows a window or runs with none (`InstanceRequest::shows`): an
    /// instance that shows nothing draws no window, offers no window verb, and runs the app's
    /// actions on its document alone.
    pub shows: Shows,
    /// Whether this run carries the package's migrations (`InstanceRequest::carries_migrations`):
    /// it carries what a native app of the package's id kept in the workspace, though it reads the
    /// workspace through a broker. It moves onto the one placement function (`work::takes_turn`)
    /// once that lands.
    pub carries_migrations: bool,
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
            access: None,
            shows: Shows::View,
            carries_migrations: false,
        }
    }

    /// [`Self::new`] for the window `person` reads at, in the language `language` names.
    #[must_use]
    pub fn in_language(mut self, language: impl Into<String>) -> Self {
        self.language = language.into();
        self
    }

    /// [`Self::new`] for a person whose access in the workspace is `access`.
    #[must_use]
    pub fn access(mut self, access: tilo_action_policy::MemberAccess) -> Self {
        self.access = Some(access);
        self
    }

    /// [`Self::new`] for a run that carries the package's migrations and shows nothing
    /// ([`Self::carries_migrations`]).
    #[must_use]
    pub fn carrying_migrations(mut self) -> Self {
        self.shows = Shows::Nothing;
        self.carries_migrations = true;
        self
    }

    /// [`Self::new`] for an instance that shows `shows`: a window, or nothing.
    #[must_use]
    pub fn showing(mut self, shows: Shows) -> Self {
        self.shows = shows;
        self
    }
}

/// The transport of an instance that shows nothing ([`Shows::Nothing`]): every batch the runtime
/// sends its window is dropped, and no window ever sent one.
#[derive(Debug, Default)]
pub struct NoView;

impl Transport for NoView {
    fn send(&mut self, _batch: Vec<u8>) -> Result<(), WireError> {
        Ok(())
    }

    fn recv(&mut self) -> Result<Option<Vec<u8>>, WireError> {
        Ok(None)
    }
}

/// The documents a native app of the package's id `app` kept in `record`'s workspace on this
/// device, when this run `carries` the package's migrations (none elsewhere), each by the id it holds (a
/// store keys a document by a name made from its id), else by the key it is stored under: its
/// records, which the package now under that id carries over once with a migration of
/// its own. They are read under `app`, not under the id of `record`'s route: a package published
/// under another id before keeps its own documents there, while the native app it took the id
/// from wrote under the id the package has now.
/// Read from the store an earlier version wrote, and never written: the host keeps no live copy
/// of a document, and this is the one remaining read of what a version before it left behind.
/// While one of them does not read, or the store cannot list them, none are handed over: carrying
/// some of them would leave the rest behind for good, so the carrying waits for a start that
/// reads them all.
fn earlier_documents(
    documents: Option<&Rc<dyn durable_store::DurableStore>>,
    app: &str,
    record: &Connection,
    carries: bool,
) -> Vec<(String, automerge::AutoCommit)> {
    let Some(documents) = documents else {
        return Vec::new();
    };
    if !carries {
        return Vec::new();
    }
    let workspace = record.route().cluster_id.to_string();
    let workspace = workspace.as_str();
    let keys = match documents.documents(app, workspace) {
        Ok(keys) => keys,
        Err(error) => {
            tracing::warn!(app, %error, "what a native app kept is not listed; carrying it waits");
            return Vec::new();
        }
    };
    let mut read = Vec::with_capacity(keys.len());
    for key in keys {
        let loaded =
            collaboration::document::save::resource_document(documents, app, workspace, &key)
                .map_err(|error| error.to_string())
                .and_then(|mut store| store.load().map_err(|error| error.to_string()));
        match loaded {
            Ok(Some(doc)) => {
                let id = collab_types::document::identity::identity(&doc)
                    .map_or(key, |held| held.id.as_str().to_owned());
                read.push((id, doc));
            }
            Ok(None) => {
                tracing::warn!(
                    app,
                    key,
                    "a document a native app kept has not arrived; carrying it waits"
                );
                return Vec::new();
            }
            Err(error) => {
                tracing::warn!(app, key, %error, "a document a native app kept does not read; carrying it waits");
                return Vec::new();
            }
        }
    }
    read
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

/// What a [`Surface`]'s resources are built from: the published documents' revision, the
/// loaded packages, the updates that saw a change, and the resources the window shows (its
/// rows, as published ids) — a change in what a person can reach publishes too.
type SurfaceInputs = (
    u64,
    Vec<tilolang_tilo::runtime::PackageId>,
    u64,
    Vec<String>,
);

/// The part of [`SurfaceInputs`] the directory depends on: the document's revision, the loaded
/// packages and the updates that saw a change. What the window shows is not in it — the
/// directory answers questions about the whole workspace, so a change that only brings other
/// rows on screen leaves it as it was.
type DirectoryInputs = (u64, Vec<tilolang_tilo::runtime::PackageId>, u64);

/// What the instance publishes on its connection: the workspace's documents with their actions, kept up
/// to date change by change.
struct Surface {
    /// The instance's app: its resource carries what every app publishes.
    app: String,
    /// "Show this window", in the person's language: a window's alone (`None` for an instance
    /// that shows nothing).
    activate: Option<tilo_actions::ActionSpec>,
    /// Whether the surface publishes the resources anyone may call on, since no view bounds what
    /// it holds: a call that shows nothing, whose caller names what it calls.
    everything: bool,
    host: Host,
    planner: Arc<dyn Planner>,
    publications: RefCell<Publications>,
}

impl Surface {
    fn new(
        app: &str,
        activate: Option<tilo_actions::ActionSpec>,
        everything: bool,
        host: &Host,
    ) -> Self {
        let handle = Arc::new(
            host.handle()
                .with_caller(|| CALLER.with(|caller| caller.borrow().clone())),
        );
        let mut publications = Publications::new(host);
        if !everything {
            // Nothing here is published up front: what an invocation names is published when it
            // names it (`addressed`). A workspace of ten thousand costs an open what it takes.
            publications.window();
        }
        let planner: Arc<dyn Planner> = handle;
        let surface = Self {
            app: app.to_owned(),
            activate,
            everything,
            host: host.clone(),
            planner,
            publications: RefCell::new(publications),
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
    /// published documents' revision, the loaded packages, the updates that saw a change, and the
    /// ids of the resources the window shows.
    fn inputs(&self, shown: &[String]) -> SurfaceInputs {
        (
            self.publications.borrow().revision(),
            self.host.package_ids(),
            self.publications.borrow().moved(),
            shown.to_vec(),
        )
    }

    /// The resources the instance publishes: the host's own resource, the workspace's documents
    /// with their actions, and each package's actions that take no document (the instance's own
    /// app's with what every app publishes).
    ///
    /// `shown` bounds what a document publishes to what the window reaches: its level's target and
    /// the rows on screen. `None` is the whole workspace, what a caller that asks the directory
    /// reads.
    ///
    /// A document is published before the package catalogue: what the window holds and the actions
    /// its own resources offer are what a person came to do, so the palette lists them first, and
    /// the verbs the platform adds for every app (`with_app_actions`) follow the app's own.
    fn resources(&self, shown: Option<&[String]>) -> Vec<ResourceSpec> {
        let mut out = vec![tilolang_tilo::publish::host_resource()];
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
                resource = with_app_actions(resource, self.activate.as_ref());
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

    /// `resources` with what the address names, added as the document publishes it when the window
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

    /// The resources an invocation of `address` resolves against: `resources` when one of them
    /// publishes it, or else every resource the workspace holds, since a caller names the document
    /// an action acts on whatever a window shows. An address no resource publishes is refused by
    /// the API as it is.
    fn naming(&self, resources: Vec<ResourceSpec>, address: &ActionAddress) -> Vec<ResourceSpec> {
        if tilo_actions::api::locate(&resources, address).is_some() {
            return resources;
        }
        let every = self.resources(None);
        if tilo_actions::api::locate(&every, address).is_some() {
            return every;
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
        personal: spec.personal,
        workspace: spec.workspace.clone(),
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

    /// The publication reads the store as it updates: nothing is read off the frame.
    fn pending(&self) -> bool {
        false
    }

    fn settle(&self) -> bool {
        self.0.update();
        false
    }

    fn visited(&self) -> u64 {
        self.0.publications.borrow().visited()
    }

    fn sweeps(&self) -> u64 {
        self.0.publications.borrow().sweeps()
    }
}

/// `resource` with what every app publishes, those it does not offer itself: the history verbs
/// of its document, `read`, and "Show this window" where the instance shows one.
fn with_app_actions(
    resource: ResourceSpec,
    activate: Option<&tilo_actions::ActionSpec>,
) -> ResourceSpec {
    let mut resource = tilolang_tilo::resource_service::with_history(resource);
    for spec in std::iter::once(tilo_resource::read::spec(None)).chain(activate.cloned()) {
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

type BuildsSlot = Rc<RefCell<Option<Arc<LaterBuilds>>>>;

/// The person's formats, as the session handed them over the host's api: shared by every
/// instance the host runs, so a change in Settings reaches them together.
pub type Formats = Rc<Cell<tilo_formats::RegionFormats>>;

/// What the instance asks of the host: its package, the platform's phrases and clock, waking the
/// host, and publishing on its connection.
struct HostPlatform {
    bundle: Arc<Bundle>,
    /// The apps installed here ([`Spec::installed`]): a package the app uses that is one of them
    /// is loaded as another app's.
    installed: BTreeMap<String, String>,
    /// Whether this instance shows a window: its surface offers "Show this window" only when one
    /// exists to bring forward.
    shows: Shows,
    /// The words of this window: its app's own and its frames', in the language the window speaks
    /// (the person's where the package ships it), so what its dates and clocks read in is the same
    /// language the toolkit's own say them in.
    phrases: Rc<Phrases>,
    /// The person's formats, which their clock and dates read by.
    formats: Formats,
    wake: Waker,
    surface: SurfaceSlot,
    /// The later builds the instance follows ([`Hosted::follow_builds`]).
    builds: BuildsSlot,
    /// The device content store of the app in its workspace, which the session opened for this
    /// instance: what a chosen file's bytes are kept in and its document's pictures are read
    /// from. `None` on a device that keeps no content store.
    content: Option<tilo_content::ContentStore>,
    /// The person's own chooser, which the session opened for this instance.
    chooser: Option<std::sync::Arc<dyn tilo_dialogs::FilePicker>>,
    /// What a person asking for a file is told when the session opened no chooser.
    no_chooser: String,
}

impl Platform for HostPlatform {
    /// The latest later build when there is one that loads, else the package the instance was
    /// started with.
    fn load(&self, host: &Host) -> u64 {
        let installed = |app: &str| self.installed.contains_key(app);
        if let Some(update) = self.builds().and_then(|builds| builds.latest()) {
            match host.load_bundle_using(&update.bundle, installed) {
                Ok(_) => return update.generation,
                Err(error) => {
                    tracing::error!(app = update.bundle.meta.app, %error, "the later build did not load");
                }
            }
        }
        if let Err(error) = host.load_bundle_using(&self.bundle, installed) {
            tracing::error!(app = self.bundle.meta.app, %error, "the package did not load");
        }
        0
    }

    fn builds(&self) -> Option<Arc<LaterBuilds>> {
        self.builds.borrow().clone()
    }

    /// Receivers decide each change against the surface of the build that runs now.
    fn surface_changed(&self, app: &str, bundle: &Bundle) {
        match bundle.surface() {
            Ok(surface) => collab_types::app_surface::register_package(app, &surface),
            // The surface registered before stays: receivers keep deciding against it.
            Err(error) => tracing::error!(app, %error, "the later build's surface is unusable"),
        }
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
        format!("{:.0}", local_now().timestamp())
    }

    /// The zone the person's clock stands in, which their day begins in: the device's, as their
    /// window's dates read it, so a day an action writes is the day their window shows it under.
    fn zone(&self) -> i64 {
        i64::from(local_now().offset().seconds())
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
                .to_monday_zero_offset(),
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
            now.date(),
            now.datetime(),
            i64::from(now.offset().seconds()),
            &*self.phrases,
        )
    }

    /// A day or a time of day named in a sentence (`"{d}"`), in the person's styles and language:
    /// a day as `Jun 29`, or `Jun 29, 2025` another year, a moment as `Jun 29, 6:00 PM`.
    fn sentence(&self, kind: &str, value: &str) -> String {
        let now = local_now();
        tilo_formats::sentence(
            kind,
            value,
            self.formats.get(),
            now.date(),
            i64::from(now.offset().seconds()),
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
        crate::files::read(self.content.as_ref()?, hash)
    }

    fn choose_file(
        &self,
        request: &tilolang_tilo::runtime::blob::FileRequest,
    ) -> Result<Option<tilolang_tilo::runtime::blob::FileValue>, String> {
        let Some(store) = self.content.as_ref() else {
            return Err(crate::files::NO_CONTENT_STORE.to_owned());
        };
        let Some(chooser) = self.chooser.as_deref() else {
            return Err(self.no_chooser.clone());
        };
        crate::files::choose(store, chooser, request)
    }

    fn save_file(
        &self,
        request: &tilolang_tilo::runtime::blob::SaveRequest,
        content: &[u8],
    ) -> Result<(), String> {
        let Some(chooser) = self.chooser.as_deref() else {
            return Err(self.no_chooser.clone());
        };
        crate::files::save(chooser, request, content)
    }

    fn keep_content(
        &self,
        name: &str,
        media: &str,
        bytes: &[u8],
    ) -> Result<tilolang_tilo::runtime::blob::FileValue, String> {
        let store = self
            .content
            .as_ref()
            .ok_or_else(|| crate::files::NO_CONTENT_STORE.to_owned())?;
        crate::files::keep(store, name, media, bytes)
    }

    fn waker(&self) -> Waker {
        Arc::clone(&self.wake)
    }

    fn publish(&self, host: &Host) -> Option<Box<dyn Publication>> {
        // A window's instance offers to bring its window forward; one that shows nothing has none
        // to bring.
        let activate = (self.shows == Shows::View).then(|| {
            tilo_actions::remote::activate_spec(
                self.phrases.say("app-activate-window", &[]),
                self.phrases.say("app-activate-window-description", &[]),
            )
        });
        // A call that shows nothing has no view to bound what it holds: it publishes the app's
        // resources, which is what a caller names when it calls.
        let everything = self.shows == Shows::Nothing;
        let surface = Rc::new(Surface::new(
            &self.bundle.meta.app,
            activate,
            everything,
            host,
        ));
        *self.surface.borrow_mut() = Some(Rc::clone(&surface));
        Some(Box::new(Held(surface)))
    }
}

/// What an instance is handed beside its store: what an earlier app of its id kept on this device,
/// and the files the session opened for it.
#[derive(Default)]
pub struct Kept {
    /// What an earlier version of the app kept on this device, for the package's one-time
    /// conversion of it (a native app's records). Read once, never written: `None` where this
    /// host was granted nothing to read.
    pub earlier: Option<Rc<dyn durable_store::DurableStore>>,
    /// The device content store of the app in its workspace, when the session opened one for
    /// this instance: the files a person chooses and the pictures its resources hold are kept
    /// there, outliving the window.
    pub content: Option<tilo_content::ContentStore>,
    /// The person's chooser, when the session opened one for this instance.
    pub chooser: Option<std::sync::Arc<dyn tilo_dialogs::FilePicker>>,
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
    /// it and whether it can run; `None` for an action whose app says nothing its target
    /// decides, which is offered as it is published.
    said: Option<tilolang_tilo::host::Offered>,
    /// The fields reading the answer read; `None` for an answer that read beyond the workspace
    /// (the clock, the window's space, the person's view state), which moves without it moving.
    reads: Option<Vec<(String, String)>>,
}

/// A row that shows what an app says of an action on it: what it is called there, the icon
/// beside it and whether it can run. The window's catalogue and the action directory carry the
/// same answer, so both are shown it the same way.
trait Offers {
    fn offers(&mut self, offered: &tilolang_tilo::host::Offered);
}

impl Offers for ActionDescriptor {
    fn offers(&mut self, offered: &tilolang_tilo::host::Offered) {
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

impl Offers for collaboration::ExportedAction {
    fn offers(&mut self, offered: &tilolang_tilo::host::Offered) {
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
            let reached_now = since
                .entry(answered.at)
                .or_insert_with(|| publications.changed_since(answered.at))
                .as_ref()
                .is_none_or(|moved| reads.iter().any(|read| moved.reaches(read)));
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
    instance: Instance,
    /// The words of the window, which the instance's platform reads: the person's language
    /// chosen again ([`Self::speak`]) relabels the window where it stands.
    phrases: Rc<Phrases>,
    connection: Connection,
    /// The workspace's store, as the instance's runtime reads and writes it.
    store: ResourceStore,
    surface: SurfaceSlot,
    builds: BuildsSlot,
    /// What the broker holds of the instance's surface, and the descriptors it was built from.
    published: Option<(Vec<ActionDescriptor>, SurfacePublication)>,
    /// The directory the instance answers its connection's directory requests from: what it
    /// last published, browsable by level and name, as the API and agents read it.
    directory: PublishedDirectory,
    /// Whether the surface moved since [`Self::directory`] was built ([`Self::publish`],
    /// [`Self::relabel`]): it is built again when it is next asked, not on every change.
    directory_behind: bool,
    /// What [`Self::directory`] was built from ([`DirectoryInputs`]): a publication whose
    /// inputs it covers leaves the directory as it was, so a workspace of ten thousand items
    /// surveys its api once, not once per row scrolled onto the screen.
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
    /// The whole index being published off the frame: where the publication's journal stood
    /// when it was read, and where the deltas that publish it arrive. Until they do, a change
    /// waits for them; it is published by the frames after.
    search_whole: Option<(
        u64,
        std::sync::mpsc::Receiver<tilolang_tilo::search::Published>,
    )>,
    /// What wakes the host: an index read off the frame wakes it too, and the read is the first
    /// frame's, before the instance published a surface with a wake of its own.
    wake: Waker,
    /// The other apps the app uses, each read through its own package ([`Used`]).
    used: Vec<Used>,
    /// What the app's views and actions asked of the other apps' apis, waiting to be sent.
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
    /// The scope the instance's code runs in, its document's content store registered for it.
    scope: DocumentScope,
    /// What the person may do in the app now: the launch's, until the session says it changed
    /// ([`Self::set_access`]). The runtime reads it at each decision.
    access: Rc<RefCell<Option<tilo_action_policy::MemberAccess>>>,
    /// The workspace's document of each person's own state: what the person picked in the views
    /// and what they have seen, given to the views and kept back as they change it.
    personal: PersonalDocument,
    /// Who else is where in the document, as the connection hears it, and what the instance last
    /// said of its own person there.
    presence: Presence,
    /// What the connection delivered before the instance ran, taken on its first frame.
    early: Vec<CollabEvent>,
    /// The file the app was opened with, until the turn that published its surface runs the action
    /// its package declares for the file's kind ([`Self::opened`]).
    opening: Option<runtime_portal::process_link::OpenedFile>,
    /// The content store the app's files are kept in, which the session opened for it: the bytes
    /// of the file its launch named are read from here, as its own `file.text()` reads them.
    files: Option<tilo_content::ContentStore>,
}

/// The wake scope an instance's code runs in, the device's content store registered for it: an
/// action too large to record whole keeps its large parts there, sealed, and every read of a
/// recorded action reads them back from there, as a native window's actions do
/// ([`tilo_actions::content::register`]). Work the code starts off its thread takes the store
/// along.
struct DocumentScope(tilo_wake::WakeScope);

impl DocumentScope {
    /// The scope of an instance's code, whose wakes wake the host through `wake`.
    fn of(content: tilo_content::ContentStore, wake: &Waker) -> Self {
        let mut scope = tilo_wake::WakeScope::new();
        let sink = Arc::clone(wake);
        if let Err(error) = scope.bind(move |_| sink()) {
            tracing::warn!(?error, "the document's wake scope did not bind");
        }
        tilo_actions::content::register(&scope.handle(), content);
        Self(scope)
    }

    /// Run the document's code from here until the guard drops.
    fn enter(&self) -> tilo_wake::WakeScopeGuard {
        self.0.enter()
    }
}

impl Hosted {
    /// The instance `spec` of `bundle` on `connection`, its resources in `store` and their content
    /// in `content`, its words read by `phrases` and written in `formats`.
    #[must_use]
    pub fn new(
        spec: Spec,
        connection: Connection,
        (store, content): (ResourceStore, tilo_content::ContentStore),
        bundle: Arc<Bundle>,
        kept: &Kept,
        window: Box<dyn Transport>,
        (phrases, formats): (&Rc<Phrases>, &Formats),
        wake: Waker,
    ) -> Self {
        let surface: SurfaceSlot = Rc::default();
        let builds: BuildsSlot = Rc::default();
        // The window speaks the person's language where its package ships it, and the package's
        // source language otherwise: the frames' words, the app's own words and the history's
        // sentences all follow that language, never the one this host's process runs in.
        let phrases = Rc::new(window_phrases(phrases, &spec, &bundle));
        // The names the session sends for the apps installed here read in the language this
        // window speaks, whatever language the workspace's people are named in.
        collaboration::history_source::authors::set_names_language(&phrases.locale());
        let scope = DocumentScope::of(content, &wake);
        let in_scope = scope.enter();
        let used = used_apps(&spec, &bundle, &store);
        let personal = PersonalDocument::open(&spec.app, &spec.person, &store);
        let platform = HostPlatform {
            bundle: Arc::clone(&bundle),
            installed: spec.installed.clone(),
            shows: spec.shows,
            phrases: Rc::clone(&phrases),
            formats: Rc::clone(formats),
            wake: Arc::clone(&wake),
            surface: Rc::clone(&surface),
            builds: Rc::clone(&builds),
            content: kept.content.clone(),
            chooser: kept.chooser.clone(),
            no_chooser: crate::files::NO_CHOOSER.to_owned(),
        };
        let mut instance = Instance::new(
            Config {
                app: spec.app.clone(),
                me: spec.person.as_str().into(),
                mode: Mode::Live,
                device: None,
                store: store.clone(),
                root: tilolang_tilo::host::RootView::default(),
                sink: None,
            },
            Rc::new(platform),
            // People by what the workspace's member list calls them, once it arrived.
            Rc::new(tilolang_tilo::names::Installed),
            window,
        );
        // The device that governs the workspace is its documents' origin: the package's migrations
        // run there, once, over what a native app of the package's id kept there too. A run that
        // reads its workspace through a broker says itself that it carries them.
        let carries = connection.governs() || spec.carries_migrations;
        instance.set_origin(carries);
        instance.set_earlier(earlier_documents(
            kept.earlier.as_ref(),
            &spec.app,
            &connection,
            carries,
        ));
        // What its runtime offers and invokes is decided by the person's access as the session
        // says it, read at each decision.
        let access = Rc::new(RefCell::new(spec.access.clone()));
        let read = Rc::clone(&access);
        instance.set_access(Rc::new(move || read.borrow().clone()));
        drop(in_scope);
        let presence = Presence::new(&spec.app, &spec.person);

        Self {
            spec,
            bundle,
            instance,
            phrases,
            connection,
            store,
            surface,
            builds,
            published: None,
            directory: PublishedDirectory::default(),
            directory_behind: false,
            directory_from: None,
            published_from: None,
            refused: Vec::new(),
            answers: Answers::default(),
            search_index: tilo_resource::search_index::Publisher::default(),
            search_index_moved: None,
            search_entries: std::collections::BTreeMap::new(),
            search_index_build: 0,
            search_whole: None,
            wake,
            used,
            asks: Rc::default(),
            asking: false,
            identity: None,
            scope,
            personal,
            access,
            presence,
            early: Vec::new(),
            opening: None,
            files: kept.content.clone(),
        }
    }

    /// Take over each later build `builds` hands over, from the first turn on: before it, as the
    /// instance loads its package.
    pub(crate) fn follow_builds(&self, builds: Arc<LaterBuilds>) {
        *self.builds.borrow_mut() = Some(builds);
    }

    /// Open `file` in the app on the turn that publishes its surface: the action its package
    /// declares for the file's kind runs with it, as a chooser hands a file to an action, and what
    /// it made opens in the window.
    pub fn open_file(&mut self, file: runtime_portal::process_link::OpenedFile) {
        self.opening = Some(file);
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

    /// Whether the instance owes its window a frame: a change it has not sent, or work it reads
    /// off the frame that changes what it shows.
    #[must_use]
    pub fn owes_a_frame(&self) -> bool {
        self.instance.owes_a_frame()
    }

    /// Whether this is a run that carries the package's migrations whose work is done: the
    /// package's migrations ran (or found their marks) and the workspace was handed what they
    /// wrote. Such a run is never a reader that stays, so it ends then.
    #[must_use]
    pub fn carried(&self) -> bool {
        self.spec.carries_migrations && self.instance.has_migrated() && !self.owes_a_frame()
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

    /// Take `events`, which the connection delivered while its route joined, on the next frame,
    /// before what arrives then: the workspace's member list, which the broker hands it once.
    pub fn arrived_early(&mut self, events: Vec<CollabEvent>) {
        self.early.extend(events);
    }

    /// How long until the instance shows something else with nothing arriving: the time a view
    /// read moves on (its midnight), or someone heard writing a message goes quiet long enough to
    /// stop reading as typing. `None` when neither waits.
    #[must_use]
    pub fn wakes_in(&self) -> Option<std::time::Duration> {
        [self.instance.wakes_in(), self.presence.wakes_in()]
            .into_iter()
            .flatten()
            .min()
    }

    /// The instance's connection to its workspace.
    #[must_use]
    pub fn connection(&self) -> &Connection {
        &self.connection
    }

    /// The workspace's store, as the instance reads and writes it.
    #[must_use]
    pub fn store(&self) -> &ResourceStore {
        &self.store
    }

    /// The package the instance runs.
    #[must_use]
    pub fn bundle(&self) -> &Bundle {
        &self.bundle
    }

    /// Keep the package's types with an origin in step with `origin`'s service.
    pub fn add_origin(&mut self, origin: tilolang_tilo::origin::SharedOrigin) {
        let _code = self.scope.enter();
        self.instance.add_origin(origin);
    }

    /// The person's environment changed — their formats, or the name the window calls someone:
    /// every view that shows what only the window knows runs again, and its window shows it.
    pub fn person_changed(&mut self) {
        let _code = self.scope.enter();
        self.instance.person_changed();
    }

    /// What the person may do in the app changed (a role given or taken back): the views offer and
    /// the runtime invokes by `access` from now on, and what they offer is drawn again. Whether
    /// it changed.
    pub fn set_access(&mut self, access: Option<tilo_action_policy::MemberAccess>) -> bool {
        if *self.access.borrow() == access {
            return false;
        }
        *self.access.borrow_mut() = access;
        self.published_from = None;
        self.answers = Answers::default();
        self.person_changed();
        true
    }

    /// The person chose `language` (`de-DE`) in Settings: the window reads its words in it where
    /// its package ships it, at once and where it stands — its views, the labels it publishes and
    /// its history's sentences. Whether its words read in another language now.
    pub fn speak(&mut self, language: &str) -> bool {
        if !self.phrases.speak(language) {
            return false;
        }
        let _code = self.scope.enter();
        let locale = self.phrases.locale();
        collaboration::history_source::authors::set_names_language(&locale);
        // Every word is read again: the views drawn, the window's own chrome and the surface
        // published.
        self.instance.speak(locale);
        self.published_from = None;
        self.answers = Answers::default();
        true
    }

    /// Run the package's scenario `scenario` in the instance on its next frame, as a capture
    /// stages one: its changes are the person's steps on the instance's document.
    pub fn stage(&mut self, scenario: &str) {
        let _code = self.scope.enter();
        self.instance.stage(Some(scenario));
    }

    /// One frame: what the connection delivered taken in, what the person did run (its writes
    /// sent to the store as it ran), what the broker delivered run, and the surface published.
    /// Whether anything changed.
    pub fn turn(&mut self) -> bool {
        let _code = self.scope.enter();
        let mut events = std::mem::take(&mut self.early);
        events.extend(self.connection.poll());
        let mut changed = !events.is_empty();
        self.instance.sync();
        // What the person picked and saw, as their own document keeps it, before the views run.
        if self.personal.take_moved(&self.store)
            && !self
                .instance
                .bring_personal(self.personal.state(), self.spec.instance)
        {
            self.personal.moved_again();
        }
        // Who else is here and who is writing, before the views that show it run.
        let before = events.len();
        events.retain(|event| !self.presence.take(event));
        changed |= events.len() != before;
        self.presence.show(&mut self.instance);
        changed |= self.instance.update();
        let chose = self.chosen();
        changed |= chose;
        if let Some(why) = self.instance.take_stage_failure() {
            tracing::error!(%why, "the staged scenario failed");
        }
        if chose {
            self.instance.show_ran();
        }
        // After what the person did before asking is written: an undo takes back their last
        // step, typing they did just before it included.
        for asked in self.instance.take_history() {
            changed |= self.take_step(&asked);
        }
        // Where the person is and whether they write there, as the views now stand.
        let connection = &self.connection;
        self.presence.say(&self.instance, |request| {
            if !connection.manager().send(request) {
                tracing::debug!("the connection did not take the person's presence");
            }
        });
        // What the person changed of their own state is kept in its document, as theirs.
        let keeps = self
            .instance
            .take_personal(self.personal.state(), self.spec.instance);
        self.personal.keep(&self.spec.person, &keeps, &self.store);
        // An invocation the broker cancelled before this frame ran it writes nothing: the caller
        // stopped waiting, so the app's code never runs for it.
        let cancelled: std::collections::HashSet<u64> = events
            .iter()
            .filter_map(|event| match event {
                CollabEvent::InvocationCancellation { id } => Some(*id),
                _ => None,
            })
            .collect();
        for event in events {
            changed |= self.delivered(event, &cancelled);
        }
        changed |= self.read_used();
        self.send_asks();
        self.publish();
        self.answer_directory();
        self.publish_search_index();
        changed |= self.opened();
        changed
    }

    /// Run the action the package declares for the file the app was opened with, once the
    /// instance has published its surface: what a person gets by opening a file in the app, as a
    /// chooser hands one to an action. What the action made opens in the window, as a press of a
    /// primary action's does. Whether a file was opened.
    fn opened(&mut self) -> bool {
        let Some(file) = self.opening.take() else {
            return false;
        };
        let opening = match &file.name {
            Some(name) => format!("`{name}`"),
            None => "the file".to_owned(),
        };
        // The action its package declares for this kind of file: the extension the name ends in,
        // which is the narrower claim, else the media type the name declares.
        let declared = self
            .bundle
            .meta
            .actions
            .iter()
            .filter(|action| !action.opens.is_empty())
            .find(|action| declared_for_file(action, &file))
            .cloned();
        let Some(action) = declared else {
            // The app does not open this kind of file: its window shows its records, and says why
            // rather than opening a window that took nothing.
            self.instance
                .failed(format!("`{}` does not open {opening}", self.spec.app));
            return true;
        };
        self.open_declared(&action, file, &opening);
        true
    }

    /// Run `action` with `file` on the root it opens into, as a view's `file.choose` runs the
    /// action it hands the file to, and open what it made. A file whose bytes are a workspace
    /// file's manifest is joined to the file it is first.
    fn open_declared(
        &mut self,
        action: &tilolang_tilo::tilolang::meta::ActionMeta,
        file: runtime_portal::process_link::OpenedFile,
        opening: &str,
    ) {
        let Some(target) = self.bundle.meta.target_of(&action.params) else {
            return;
        };
        let media =
            tilolang_tilo::runtime::blob::media_of(file.name.as_deref().unwrap_or_default());
        // What a person's file is: its bytes, or the chunks of a workspace file the store keeps a
        // manifest of, joined as the file they are.
        let Some(content) = self.files.as_ref() else {
            self.instance
                .failed(crate::files::NO_CONTENT_STORE.to_owned());
            return;
        };
        let max = u64::try_from(tilolang_tilo::runtime::blob::MAX_CHOSEN).unwrap_or(u64::MAX);
        let bytes = content.read(&file.hash).and_then(|read| {
            ui::content::manifest::whole(read, max, |chunk| {
                let mut address = String::with_capacity(64);
                for byte in chunk {
                    let _ = write!(address, "{byte:02x}");
                }
                content.read(&address)
            })
        });
        let Some(bytes) = bytes else {
            self.instance.failed(format!("{opening} could not be read"));
            return;
        };
        let opened = tilolang_tilo::runtime::blob::FileValue::new(
            file.name.unwrap_or_default(),
            media,
            i64::try_from(bytes.len()).unwrap_or(i64::MAX),
            file.hash,
        );
        let Some(host) = self.instance.host() else {
            return;
        };
        let root = match host.root_resource(&self.spec.app, &target.id) {
            Ok(root) => root,
            Err(error) => {
                self.instance.failed(error.to_string());
                return;
            }
        };
        let actor = tilolang_tilo::runtime::Actor::Member(MemberId::person(&self.spec.person));
        let outcome = host.runtime().invoke_action(
            &actor,
            (
                action.id.as_str(),
                vec![Value::resource(&root), Value::file(opened)],
            ),
        );
        let made = match outcome {
            Ok(InvokeOutcome::Done(Value::Ref(made))) => {
                Some(std::rc::Rc::unwrap_or_clone(made.target))
            }
            Ok(InvokeOutcome::Done(_)) => None,
            Ok(other) => {
                self.instance
                    .failed(format!("{opening} was not opened: {other:?}"));
                return;
            }
            Err(error) => {
                self.instance.failed(error.to_string());
                return;
            }
        };
        if let Some(made) = made {
            self.instance
                .opens_what_it_made(tilolang_tilo::publish::resource_id(
                    &self.spec.app,
                    &made.id_string(),
                ));
        }
    }

    /// One history ask carried out on the store: the person's step taken back or brought back,
    /// or answered with why it cannot be. Whether what the instance holds changed.
    fn take_step(&mut self, asked: &tilolang_tilo::instance::HistoryAsk) -> bool {
        use tilolang_tilo::instance::HistoryAsk;
        let edit = match asked {
            HistoryAsk::Undo => HistoryEdit::Undo {
                steps: Some(1),
                invocation: None,
                by: None,
                to: None,
                step: None,
            },
            HistoryAsk::Redo => HistoryEdit::Redo { steps: Some(1) },
            HistoryAsk::UndoStep(step) | HistoryAsk::UndoRun(step) => HistoryEdit::Undo {
                steps: None,
                invocation: None,
                by: None,
                to: None,
                step: Some(step.clone()),
            },
        };
        let by = tilolang_tilo::runtime::Actor::Member(MemberId::person(self.spec.person.as_str()));
        match self.history_edit(&edit, &by) {
            Ok(_) => {
                self.store_moved();
                true
            }
            Err(error) => {
                tracing::warn!(app = self.spec.app, ?asked, %error, "an undo or redo failed");
                self.instance.failed(error);
                true
            }
        }
    }

    /// The workspace's resources moved, here or in another device's window: what the instance
    /// read of the store is read again, and the other apps whose resources it reads are read
    /// again too, so what it shows moves with them.
    fn store_moved(&mut self) {
        self.instance.store_moved();
        for used in &mut self.used {
            used.moved = true;
        }
    }

    /// Carry out the history verb `edit` on the store, as `by`'s: undo takes back the last step
    /// written from this instance (or the invocation it names, everywhere it wrote), redo brings
    /// back what the last undo took back. What the verb did, as it answers its caller.
    fn history_edit(
        &self,
        edit: &HistoryEdit,
        by: &tilolang_tilo::runtime::Actor,
    ) -> Result<Option<serde_json::Value>, String> {
        let failed = |error: tilolang_tilo::runtime::store::StoreError| error.to_string();
        match edit {
            HistoryEdit::Undo {
                invocation, step, ..
            } if invocation.is_some() || step.is_some() => {
                let named = invocation
                    .as_ref()
                    .or(step.as_ref())
                    .cloned()
                    .unwrap_or_default();
                self.store.undo(&named, Some(by)).map_err(failed)?;
                Ok(Some(serde_json::json!({ "undone": [named] })))
            }
            HistoryEdit::Undo { steps, .. } => {
                let mut undone = 0_u32;
                for _ in 0..steps.unwrap_or(1).max(1) {
                    if !self.store.undo_last(Some(by)).map_err(failed)? {
                        break;
                    }
                    undone = undone.saturating_add(1);
                }
                if undone == 0 {
                    return Err("there is nothing to undo".to_owned());
                }
                Ok(Some(serde_json::json!({ "undone": undone })))
            }
            HistoryEdit::Redo { steps } => {
                let mut redone = 0_u32;
                for _ in 0..steps.unwrap_or(1).max(1) {
                    if !self.store.redo_last(Some(by)).map_err(failed)? {
                        break;
                    }
                    redone = redone.saturating_add(1);
                }
                if redone == 0 {
                    return Err("there is nothing to redo".to_owned());
                }
                Ok(Some(serde_json::json!({ "redone": redone })))
            }
            HistoryEdit::Restore { .. }
            | HistoryEdit::RestoreReviewed { .. }
            | HistoryEdit::Checkpoint { .. } => Err(format!(
                "{} is not offered for this app's resources",
                edit.verb()
            )),
        }
    }

    /// Publish the app's resources to the session's persistent search index, over the
    /// instance's certified route: what search finds of the room (a note's title, a message's
    /// sender, subject and body), keyed by each resource's id, so a hit opens the app at it.
    /// Nothing is sent while nothing moved; a new route sends the whole set again, and so does a
    /// change the publication's journal no longer reaches. Otherwise a change sends the entries
    /// of the resources it moved, and drops those of the resources it deleted: a task checked in
    /// a book of ten thousand replaces one entry.
    fn publish_search_index(&mut self) {
        if self.connection.route().app_id != self.spec.app {
            return;
        }
        let Some(host) = self.instance.host() else {
            return;
        };
        if let Some((moved, taken)) = self.search_whole.take() {
            match taken.try_recv() {
                Ok(published) => {
                    self.search_entries = published.entries;
                    self.search_index = published.publisher;
                    for bytes in published.deltas {
                        self.connection.manager().send(
                            collaboration::CollabRequest::PublishSearchIndex { delta_json: bytes },
                        );
                    }
                    self.search_index_moved = Some(moved);
                    return;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    self.search_whole = Some((moved, taken));
                    return;
                }
                // The thread ended without publishing it: it is read whole again.
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {}
            }
        }
        let Some(surface) = self.surface.borrow().clone() else {
            return;
        };
        // What the workspace moved since the index last took it, read from the publication the
        // window reads rather than from the document: a change becomes a publication at the frame
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
        let Some(tilolang_tilo::publish::TextMoves::These { texts, gone, whole }) = moved else {
            self.read_index_whole(&host);
            return;
        };
        if texts.is_empty() && gone.is_empty() && whole.is_empty() {
            return;
        }
        let state = self.index_state();
        let (upsert, mut remove) = self.moved_entries(&host, &texts, &gone);
        // A document read again whole names every text it still holds: an entry under it that
        // it no longer names is gone.
        for package in host.packages() {
            for doc in &whole {
                let id = tilolang_tilo::publish::resource_id(&package.meta.app, doc);
                let named: std::collections::BTreeSet<String> = texts
                    .iter()
                    .map(|path| tilolang_tilo::publish::resource_id(&package.meta.app, path))
                    .collect();
                // A paragraph's entry stands while the resource it is in is named: what the
                // resource holds now was published with it.
                let vanished: Vec<String> = self
                    .entries_under(&id)
                    .into_iter()
                    .filter(|key| !named.contains(tilolang_tilo::search::split_anchor(key).0))
                    .collect();
                for key in vanished {
                    self.search_entries.remove(&key);
                    remove.push(key);
                }
            }
        }
        let Some(deltas) = self.search_index.publish_moved(state, upsert, remove) else {
            self.read_index_whole(&host);
            return;
        };
        for delta in deltas {
            let Some(bytes) = delta.encode() else {
                continue;
            };
            self.connection
                .manager()
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

    /// Read every entry of the workspace and send it: what a first publication, a new route and
    /// a change the journal no longer reaches send. The entries are read from the store on the
    /// frame; the deltas that publish ten thousand of them are made off it, and a later frame
    /// sends them ([`Self::search_whole`]).
    fn read_index_whole(&mut self, host: &Host) {
        let publisher = std::mem::take(&mut self.search_index);
        let state = self.index_state();
        let moved = self.moved();
        let entries = tilolang_tilo::search::whole(host);
        let (sent, taken) = std::sync::mpsc::channel();
        let wake = Arc::clone(&self.wake);
        let spawned = std::thread::Builder::new()
            .name("tilolang-index".to_owned())
            .spawn(move || {
                let index = tilolang_tilo::search::publish_whole(publisher, state, entries);
                // An instance that ended meanwhile takes nothing, and needs no frame.
                if sent.send(index).is_ok() {
                    wake();
                }
            });
        match spawned {
            Ok(_) => self.search_whole = Some((moved, taken)),
            // Nothing was published: the next frame reads the whole index again.
            Err(error) => tracing::warn!(%error, "the search index could not be published"),
        }
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
        descriptor.offers(&offered);
    }

    /// Dim, for the person at the window, an action the rules of a document that holds its own
    /// keys deny them, saying so in their words. One the rules answer Ask stays pressable: the
    /// press stops on the Ask bar.
    fn apply_document_grants(&self, descriptors: &mut [ActionDescriptor]) {
        use tilo_action_policy::{Answer, Context, TreePath};

        let route = self.connection.route();
        let document = route.document.as_deref().unwrap_or("");
        if !collab_types::validator::requires_independent_document_keys(&route.app_id, document) {
            return;
        }
        let access = self.access.borrow();
        let Some(access) = &*access else {
            return;
        };
        let app = TreePath::workspace(route.cluster_id.to_string()).app(&route.app_id);
        let context = Context::at_time(tilo_clock::wall());
        for descriptor in descriptors {
            let need = descriptor.contract.need();
            let node = app
                .clone()
                .kind(&descriptor.resource_kind)
                .resource(document)
                .field(need.field.as_deref().unwrap_or(""))
                .action(&descriptor.address.action);
            if access.answer(&node, need.kind, &context) == Answer::Deny {
                descriptor.contract.availability = tilo_actions::Availability::Unavailable {
                    code: tilo_actions::Availability::NOT_ALLOWED.to_owned(),
                    reason: self.phrases.say("tilolang-ask-denied", &[]),
                };
            }
        }
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
                let id = tilolang_tilo::publish::resource_id(&package.meta.app, path);
                let entries = tilolang_tilo::search::index_entries_at(host, &package.meta, path);
                // What the resource published before and no longer does goes: a paragraph
                // deleted, or the whole resource once it holds no text, though it is still
                // there. What is published under it is the entries of its own resources.
                let before: Vec<String> = self
                    .entries_under(&id)
                    .into_iter()
                    .filter(|key| tilolang_tilo::search::keys_resource(key, &id))
                    .collect();
                for key in before {
                    if !entries.iter().any(|entry| entry.resource == key) {
                        self.search_entries.remove(&key);
                        remove.push(key);
                    }
                }
                for entry in entries {
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
                for key in self.entries_under(&id) {
                    self.search_entries.remove(&key);
                    remove.push(key);
                }
            }
        }
        (upsert, remove)
    }

    /// The keys of the index entries of `id`: its own, its paragraphs' and those of the resources
    /// published under it, and theirs.
    fn entries_under(&self, id: &str) -> Vec<String> {
        let under = format!("{id}/");
        self.search_entries
            .range(id.to_owned()..)
            .take_while(|(key, _)| key.starts_with(id))
            .map(|(key, _)| key.clone())
            .filter(|key| tilolang_tilo::search::keys_resource(key, id) || key.starts_with(&under))
            .collect()
    }

    /// Offer the instance's action directory on its connection, once it is connected, and
    /// answer what was asked of it since the last frame from what the instance published.
    fn answer_directory(&mut self) {
        let requests = {
            let mut manager = self.connection.manager();
            manager.publish_action_directory(self.spec.app.clone());
            manager.take_action_directory_requests()
        };
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
                    descriptor.offers(&offered);
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
        let descriptors = tilo_actions::api::survey(&resources);
        self.directory.publish(&resources, &descriptors);
    }

    /// Run what the broker delivered on the instance's connection. Every event that waits on an
    /// answer from the instance is answered: an invocation with its outcome, a handoff step with
    /// what the host does of it. The rest says something the instance does not keep (presence,
    /// cursors, results of requests it never makes), but for what it names: what each app
    /// installed here is called rides with the context, and is what this window's history reads
    /// another app's change as.
    fn delivered(
        &mut self,
        event: CollabEvent,
        cancelled: &std::collections::HashSet<u64>,
    ) -> bool {
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
            } => {
                // The caller stopped waiting before this frame ran it: the app's code does not
                // run, nothing is written, and the broker, which cancelled it, expects no answer.
                if cancelled.contains(&id) {
                    tracing::debug!(id, "an invocation cancelled before it ran is not run");
                    return false;
                }
                (id, invocation, resource, action, argument, actor, context)
            }
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
                // there: a cancellation that was not in the frame its invocation was stops
                // nothing.
                tracing::debug!(id, "a cancellation of an invocation already answered");
                return false;
            }
            CollabEvent::WorkspaceControl { .. } => {
                tracing::warn!(
                    app = self.spec.app,
                    instance = self.spec.instance,
                    "a task or workspace request reached an app instance, which does not run them"
                );
                return false;
            }
            CollabEvent::ResourcesArrived { .. } => {
                // Documents of the workspace the instance reads arrived here from elsewhere:
                // what it read of the store is read again, and what it shows moves with it.
                self.store_moved();
                return true;
            }
            _ => return false,
        };
        let address = ActionAddress::new(resource, action);
        // The invoker is who the broker admitted, with the member and the person it acts
        // for that the broker set in the delivery's context; nothing else is trusted.
        let invoker = ActionInvoker::delivered(&actor, context.as_deref());
        // A member's own document is that person's alone: anyone else is refused before anything
        // of it is read.
        let person = invoker
            .acting
            .by
            .member_id()
            .and_then(MemberId::person_id)
            .or_else(|| invoker.acting.acted_for_person());
        let route = self.connection.route();
        let profile_record = route
            .document
            .as_deref()
            .and_then(collab_types::person_document_context);
        let reaches = profile_record.is_none()
            || person.and_then(collab_types::personal_cluster_of) == Some(route.cluster_id);
        if !reaches {
            let update = failed(
                invocation,
                Refused::new("permission_denied", "this is another member's own document"),
            );
            self.answer_invocation(id, update);
            return true;
        }
        let attributed = context
            .as_deref()
            .map(|context| context.clone().with_invoker(invoker.clone()));
        // What the action writes is stamped with the invocation the broker admitted, so the
        // workspace's history tells the step by the call that made it.
        self.store.serve(Some(invocation.0.to_string()));
        let outcome = self.invoke(&address, argument.as_deref(), &invoker, attributed.as_ref());
        self.store.serve(None);
        let update = match outcome {
            Ok(output) => InvocationUpdate::Succeeded { invocation, output },
            Err(error) => failed(invocation, error),
        };
        // The action's writes reached the store as it ran: what it queued outside the workspace
        // waits in the outbox, and the instance settles it on its next frame, as it does what a
        // person's own action queues.
        self.instance.commit_planned();
        self.answer_invocation(id, update);
        true
    }

    /// Bring the other apps' readings up to date: what their documents took in is read again, and
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
            runtime.set_foreign_invoker(Apis(Rc::clone(&self.asks)));
            self.asking = true;
        }
        let mut moved = false;
        for used in &mut self.used {
            if std::mem::take(&mut used.moved) {
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
    /// apis, each as that app's action on its resource: a change comes back as its next reading.
    fn send_asks(&self) {
        let asks = std::mem::take(&mut *self.asks.borrow_mut());
        for invocation in asks {
            let Some((app, resource, action, argument)) = api_ask(&invocation) else {
                tracing::warn!(
                    app = self.spec.app,
                    ?invocation,
                    "a change of another app names no resource of it"
                );
                continue;
            };
            let request = tilo_actions::remote::InvokeRequest {
                invocation: tilo_actions::InvocationId::new(),
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
                signed: None,
            };
            let message = tilo_actions::remote::RemoteActionMessage::Invoke(request);
            if let Err(error) = self
                .connection
                .manager()
                .try_send(CollabRequest::RemoteAction { message })
            {
                tracing::warn!(app = self.spec.app, to = app, %error, "a change of another app did not reach its api");
            }
        }
    }

    /// Hand the broker the outcome of the invocation `id`. An answer the connection does not
    /// take leaves its invoker waiting until the broker ends the invocation with the connection:
    /// that is said here.
    fn answer_invocation(&self, id: u64, update: InvocationUpdate) {
        if let Err(error) = self
            .connection
            .manager()
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

    /// Answer the shell's handoff step `request`: a checkpoint writes the document's on-disk copy
    /// and is done; a retirement is refused with why, as the tilolang host does not move a
    /// document to another workspace; an owner view is presentation the instance has no window
    /// of its own for, and wants no answer.
    fn handoff(&self, request: &collaboration::handoff::HandoffRequest) {
        use collaboration::handoff::{HandoffReport, HandoffRequest};
        let report = match request {
            // Every write is in the store as it is made: nothing waits to be kept.
            HandoffRequest::Checkpoint { token } => HandoffReport::Done {
                token: *token,
                heads: Vec::new(),
            },
            HandoffRequest::Retire { token, .. } => {
                tracing::warn!(
                    app = self.spec.app,
                    instance = self.spec.instance,
                    "the shell asked a tilolang instance to retire its document, which it cannot"
                );
                HandoffReport::Failed {
                    token: *token,
                    reason: "a tilolang app's instance does not move its document to another \
                             workspace"
                        .to_owned(),
                }
            }
            HandoffRequest::OwnerView { .. } => return,
        };
        if let Err(error) = self
            .connection
            .manager()
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

    /// What a press of a control that names a flag field asks the document's own edit of it, found
    /// as `edit` (a box a view binds to a field and gives no action of its own): to mark the field
    /// the other way. `None` where `edit` names no document, or `field` is not a flag of it.
    fn toggled(&self, edit: &ActionAddress, field: &str) -> Option<String> {
        let host = self.instance.host()?;
        let read = host
            .read_content(Some(&edit.resource), &host.reads_for(host.viewer()))
            .ok()?;
        let package = host.package(&self.spec.app)?;
        let field = package
            .meta
            .type_named(read.get("type")?.as_str()?)?
            .fields
            .iter()
            .find(|declared| declared.id == field)?;
        let on = read.get("fields")?.get(&field.name)?.as_bool()?;
        let arguments = serde_json::json!({ "field": field.id, "value": !on });
        Some(format!(
            "{}{arguments}",
            tilo_actions::ARGUMENTS_JSON_PREFIX
        ))
    }

    /// The action `address` runs, built through the specs the window publishes: the one it names,
    /// or — where it names a field of a document instead (a box the view binds to the field and gives
    /// no action of its own) — the document's own edit of that field, found as any action on the
    /// document is, with the value a press of it gives.
    fn encode_named(
        &self,
        surface: &Surface,
        shown: &[String],
        address: &ActionAddress,
        argument: Option<&str>,
    ) -> (ActionAddress, Result<EncodedAction, tilo_actions::ApiError>) {
        let named_of = |address: &ActionAddress| {
            surface.naming(surface.addressed(shown, &address.resource), address)
        };
        let resources = named_of(address);
        let encoded = encode(&resources, address, argument);
        if encoded
            .as_ref()
            .is_err_and(|error| matches!(error, tilo_actions::ApiError::Unknown(_)))
        {
            let edit = ActionAddress {
                action: tilolang_tilo::action::EDIT_FLAG_ACTION.to_owned(),
                ..address.clone()
            };
            let resources = named_of(&edit);
            if let Some(arguments) = self.toggled(&edit, &address.action)
                && let Ok(run) = encode(&resources, &edit, Some(&arguments))
            {
                return (edit, Ok(run));
            }
        }
        (address.clone(), encoded)
    }

    /// Build the action `address` names for `invoker`, through the spec the instance publishes
    /// now, and run it: building it runs it, as the invoker's step, and its writes go to the
    /// store; the host's own vocabulary is answered by the runtime, as the invoker's.
    fn invoke(
        &mut self,
        address: &ActionAddress,
        argument: Option<&str>,
        invoker: &ActionInvoker,
        ambient: Option<&ActionContext>,
    ) -> Result<tilo_actions::ActionOutput, Refused> {
        let surface = self
            .surface
            .borrow()
            .clone()
            .ok_or_else(|| Refused::new("action_unavailable", "the app has not started yet"))?;
        let shown = self.shown();
        let by_person = invoker.acting.is_person();
        let caller = Caller::from(invoker);
        let host = self
            .instance
            .host()
            .ok_or_else(|| Refused::new("action_unavailable", "the app has not started yet"))?;
        // The action runs as it is built: the run that builds it is the one whose writes the
        // store takes.
        CALLER.with(|slot| *slot.borrow_mut() = Some(caller.clone()));
        // Work the caller groups as one (an agent's session) is what the action's changes are
        // steps of: they say so, and a review of that work lists them.
        let run = ambient
            .and_then(|ambient| ambient.group.as_ref())
            .map(|group| tilolang_tilo::runtime::store::Run {
                id: group.id.clone(),
                title: group.label.clone().unwrap_or_default(),
            });
        // Held to the end of the invocation, whichever way it ends: what a host verb it names
        // writes after it is built (a block run) is a step of the work too.
        let _run = run.map(|run| InRun::new(&self.store, run));
        // What the action writes is told apart from what was written before it.
        let _ = self.store.take_wrote("");
        let (address, encoded) = host.carrying_out(&invoker.acting.by, || {
            self.encode_named(&surface, &shown, address, argument)
        });
        CALLER.with(|slot| *slot.borrow_mut() = None);
        let encoded = encoded.map_err(|error| Refused::new(api_code(&error), error.to_string()))?;
        if encoded.type_id == tilo_actions::resource::INVALID_ARGUMENTS_TYPE {
            return Err(Refused::new(
                "action_invalid_arguments",
                String::from_utf8_lossy(&encoded.payload),
            ));
        }
        if encoded.type_id == EDIT_TYPE {
            // A history verb, on the document as the invoker's own step.
            let edit: HistoryEdit = serde_json::from_slice(&encoded.payload)
                .map_err(|error| Refused::new("action_invalid_arguments", error.to_string()))?;
            let value = self
                .history_edit(&edit, &invoker.acting.by)
                .map_err(|error| Refused::new("action_failed", error))?;
            self.store_moved();
            return Ok(tilo_actions::ActionOutput {
                value,
                resources: Vec::new(),
            });
        }
        if encoded.type_id == VIEW_TYPE {
            let locale = self.phrases.locale();
            return Self::history_view(
                &surface,
                &shown,
                &address,
                &encoded.payload,
                &host,
                &locale,
            );
        }
        if encoded.type_id == tilo_resource::read::TYPE {
            return self
                .read(&encoded.payload, &caller.reader())
                .map_err(|error| Refused::new("action_failed", error));
        }
        if encoded.type_id == tilo_actions::remote::APPLICATION_ACTIVATE_TYPE {
            self.instance.activate_window();
            return Ok(tilo_actions::remote::activation_requested());
        }
        if encoded.type_id == ACTION_TYPE {
            let action: DynAction = serde_json::from_slice(&encoded.payload)
                .map_err(|error| Refused::new("action_invalid_arguments", error.to_string()))?;
            if let Some(why) = self.store.take_refusal() {
                return Err(Refused::new("action_failed", why));
            }
            // The answer names the characters it wrote: the window draws them in the writer's
            // colour, ending at the writer's caret.
            let mut output = returned(&host, &action);
            if let Some(wrote) = self.store.take_wrote(&action.app) {
                output.value = tilo_actions::ActionOutput::noting_wrote(output.value, &wrote);
            }
            return Ok(output);
        }
        if address.resource == HOST_RESOURCE
            && encoded.type_id == <HostEffect as tilo_resource::Effect>::OPERATION
        {
            let effect = <HostEffect as tilo_resource::Effect>::from_payload(&encoded.payload)
                .map_err(|error| Refused::new("action_invalid_arguments", error.to_string()))?;
            let output = host
                .answer(effect, Some(&caller))
                .map_err(|error| Refused::new("action_failed", error))?;
            if let Some(why) = self.store.take_refusal() {
                return Err(Refused::new("action_failed", why));
            }
            return Ok(output);
        }
        if encoded.type_id == <OpenEffect as tilo_resource::Effect>::OPERATION {
            return self.open_from_action(&encoded.payload, by_person);
        }
        Err(Refused::new(
            "action_unknown",
            format!("the tilolang host does not run `{}`", encoded.type_id),
        ))
    }

    fn open_from_action(
        &mut self,
        payload: &[u8],
        by_person: bool,
    ) -> Result<tilo_actions::ActionOutput, Refused> {
        // A search hit, or an agent's `open`: the window shows what it names.
        let open = <OpenEffect as tilo_resource::Effect>::from_payload(payload)
            .map_err(|error| Refused::new("action_invalid_arguments", error.to_string()))?;
        self.instance.open_requests().borrow_mut().push(open);
        // A person who chose it comes to see it: the window showing it comes forward. An
        // agent's `open` leaves the person where they are.
        if by_person {
            self.instance.activate_window();
        }
        Ok(tilo_actions::ActionOutput::default())
    }

    /// Answer a history view of the resource `address` names: the versions the store holds of
    /// it and of what it is published with, oldest first, as many as it asks for, each with the
    /// sentence it reads as in `locale`.
    fn history_view(
        surface: &Surface,
        shown: &[String],
        address: &ActionAddress,
        payload: &[u8],
        host: &Host,
        locale: &str,
    ) -> Result<tilo_actions::ActionOutput, Refused> {
        use collaboration::history_source::verbs::HistoryView;
        let view: HistoryView = tilo_actions::decode_json(payload)
            .map_err(|error| Refused::new("action_invalid_arguments", error.to_string()))?;
        let resources = surface.resources(Some(shown));
        let tree = resources
            .iter()
            .find(|tree| tree.id.as_str() == address.resource)
            .ok_or_else(|| {
                Refused::new(
                    "action_unavailable",
                    format!("{} keeps no history here", address.resource),
                )
            })?;
        // The app's own resource has the history of everything the app keeps.
        let trees: Vec<&ResourceSpec> = if address
            .resource
            .strip_prefix("tl:")
            .is_some_and(|app| !app.contains(':'))
        {
            resources.iter().collect()
        } else {
            vec![tree]
        };
        let mut value = tilolang_tilo::resource_service::history(host, &trees, &view)
            .map_err(|error| Refused::new("action_failed", error))?;
        say_versions(&mut value, &resources, locale);
        Ok(tilo_actions::ActionOutput {
            value: Some(value),
            resources: Vec::new(),
        })
    }

    /// Answer `read` (`tilo_resource::read`) for `reader`: what the store holds that they may
    /// read, through the runtime.
    fn read(
        &self,
        payload: &[u8],
        reader: &tilolang_tilo::runtime::Actor,
    ) -> Result<tilo_actions::ActionOutput, String> {
        let args: serde_json::Value = if payload.is_empty() {
            serde_json::Value::Null
        } else {
            tilo_actions::decode_json(payload).map_err(|error| error.to_string())?
        };
        let item = args.get("item").and_then(serde_json::Value::as_str);
        // A version read as `null` is no version: the caller read the resource as it is now.
        if args
            .get("version")
            .is_some_and(|version| !version.is_null())
        {
            return Err(
                "a past version of this app's resources is read from its versions".to_owned(),
            );
        }
        let host = self
            .instance
            .host()
            .ok_or_else(|| "the app has not started yet".to_owned())?;
        let content = host.read_content(item, &host.reads_for(reader))?;
        // A whole resource reads a page at a time, as every app's does.
        let value = if item.is_none() {
            tilo_resource::read::page(
                content,
                args.get("cursor").and_then(serde_json::Value::as_str),
                args.get("pageSize").and_then(serde_json::Value::as_i64),
            )?
        } else {
            content
        };
        Ok(tilo_actions::ActionOutput {
            value: Some(value),
            resources: Vec::new(),
        })
    }

    /// The resources the window shows, as published ids, with the resources that hold them: a
    /// row's actions are published on its own resource, and a resource publishes with what
    /// holds it (the document's tree). What the surface a window reads is bounded by.
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
        // with ten thousand documents or items than with one.
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
        let resources = if surface.everything {
            surface.resources(None)
        } else {
            surface.resources(Some(&shown))
        };
        let handlers: HashSet<String> = HashSet::from([
            <HostEffect as tilo_resource::Effect>::OPERATION.to_owned(),
            <OpenEffect as tilo_resource::Effect>::OPERATION.to_owned(),
            VIEW_TYPE.to_owned(),
            EDIT_TYPE.to_owned(),
            tilo_resource::read::TYPE.to_owned(),
            tilo_actions::remote::APPLICATION_ACTIVATE_TYPE.to_owned(),
        ]);
        let registry = tilo_actions::ActionRegistry::with(tilolang_tilo::action::register_actions);
        let violations = tilo_actions::validate_surface(&resources, &registry, &handlers);
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
            if !self
                .published
                .as_ref()
                .is_some_and(|(held, _)| held.is_empty())
            {
                say_catalogue(&mut self.instance, &[], |_| true, false);
            }
            if self
                .published
                .as_ref()
                .is_some_and(|(held, _)| !held.is_empty())
            {
                let (delta, state) = SurfacePublication::next(
                    self.published.as_ref().map(|(d, s)| (s, d.as_slice())),
                    &[],
                );
                if self.send_surface(delta) {
                    self.published = Some((Vec::new(), state));
                    self.published_from = None;
                }
            }
            self.refused = violations;
            return;
        }
        self.refused.clear();
        let mut descriptors = tilo_actions::api::survey(&resources);
        self.answer_what_the_window_shows(&mut descriptors);
        self.apply_document_grants(&mut descriptors);
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
        self.offer(&delta, &descriptors);
        if delta.is_empty() || self.send_surface(delta) {
            self.published = Some((descriptors, state));
            self.published_from = Some((surface, inputs));
        } else {
            // A surface the connection did not take is offered again on the next frame, whole:
            // the window took this change, and what the broker holds is older.
            self.published = None;
            self.published_from = None;
        }
    }

    /// Offer the window the person's command palette `descriptors`, the actions the instance
    /// publishes on its connection: the window offers them the way it offers its own, with the
    /// labels, keys and icons the app gives them. The window holds what the broker holds, so it
    /// is sent what `delta` changes of that; all of it when the broker starts over.
    fn offer(&mut self, delta: &SurfaceDelta, descriptors: &[ActionDescriptor]) {
        if delta.reset {
            say_catalogue(&mut self.instance, descriptors, |_| true, false);
            return;
        }
        if delta.is_empty() {
            return;
        }
        let changed: HashSet<&str> = delta
            .resources
            .iter()
            .map(|resource| resource.resource.as_str())
            .collect();
        say_catalogue(
            &mut self.instance,
            descriptors,
            |resource| changed.contains(resource),
            true,
        );
    }

    /// Run the next action the person chose at the window as their own step: the same api an
    /// invocation the broker delivered for them takes, so the action runs for them however it
    /// reached the app. One a frame, so what a press opened is sent, and the window hears it
    /// ran, before the press after it ([`tilolang_tilo::instance::Instance::show_ran`]).
    /// Whether there was one.
    fn chosen(&mut self) -> bool {
        let Some((press, address, argument)) = self.instance.take_chosen() else {
            return false;
        };
        let invoker = ActionInvoker::person(self.spec.person.clone());
        let opens_what_it_made = self.is_primary(&address);
        match self.invoke(&address, argument.as_deref(), &invoker, None) {
            Ok(output) => {
                // What a press made shows, as the person asked for it: a primary action's opens
                // beside the list as a page of its own where the window has no place for its
                // kind, and any other action's where the window shows what it made — a note made
                // under the open one, or a page imported, opens in the notebook's tree.
                for anchor in output.resources {
                    if opens_what_it_made {
                        self.instance.opens_what_it_made(anchor);
                    } else {
                        self.instance.shows_what_it_made(anchor);
                    }
                }
            }
            Err(refused) => self.instance.failed(refused.message),
        }
        // The press ran: once it is in the document this frame sends what it opened, then tells
        // the window it ran, so what the person typed after it goes there.
        self.instance.press_ran(press);
        true
    }

    /// Whether the app declares the action `address` names `primary`: what it made opens as a
    /// page of its own when a person runs it, whether from a press or from the palette.
    fn is_primary(&self, address: &ActionAddress) -> bool {
        self.instance
            .host()
            .and_then(|host| host.published_action(&address.resource, &address.action))
            .is_some_and(|action| tilolang_tilo::surface::is_primary(&action))
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
        say_catalogue(
            &mut self.instance,
            descriptors,
            |resource| changed.contains(&resource),
            true,
        );
        if !delta.is_empty() && !self.send_surface(delta) {
            // What the broker holds is no longer what was patched: a later turn sends the
            // surface whole, as a reset.
            self.published = None;
            self.published_from = None;
            return true;
        }
        self.published_from = Some((Rc::clone(surface), inputs));
        true
    }

    /// Hand the broker `delta` of the instance's surface. Whether it took it: a document on this
    /// device alone has no broker, and its window's catalogue is all there is to publish to.
    fn send_surface(&self, delta: SurfaceDelta) -> bool {
        self.connection.is_local()
            || self
                .connection
                .manager()
                .send(CollabRequest::PublishActionSurface { delta })
    }

    /// What the instance holds is in the store as it is written: nothing waits as it ends.
    pub fn flush(&mut self) {}

    /// What the workspace's document of each person's own state holds, as this instance holds it.
    #[must_use]
    pub fn personal(&self) -> &tilolang_tilo::personal::PersonalState {
        self.personal.state()
    }
}

/// Where the runtime sends what a view or an action changes of another app's resources: queued
/// for [`Hosted::send_asks`], and done here; the change comes back as the resource's next reading.
struct Apis(Rc<RefCell<Vec<tilolang_tilo::runtime::delegate::Invocation>>>);

impl tilolang_tilo::runtime::delegate::Invoker for Apis {
    fn invoke(
        &self,
        invocation: &tilolang_tilo::runtime::delegate::Invocation,
    ) -> tilolang_tilo::runtime::delegate::Invoked {
        self.0.borrow_mut().push(invocation.clone());
        tilolang_tilo::runtime::delegate::Invoked::Sent
    }
}

/// `invocation` as its app's api takes it: the app, the resource, the action and its
/// arguments. An action is invoked on its target, which its arguments leave out; an edit of a
/// field is the resource's own field edit. `None` for an action that names no resource.
fn api_ask(
    invocation: &tilolang_tilo::runtime::delegate::Invocation,
) -> Option<(String, String, String, tilo_actions::remote::InvokeArgument)> {
    use tilolang_tilo::runtime::Value;
    use tilolang_tilo::runtime::delegate::Invocation;
    match invocation {
        Invocation::Action {
            app,
            name,
            target: Some(target),
            from,
            ..
        } => {
            let mut arguments = invocation.api_arguments();
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
            // The app's api takes it by the name it publishes it under.
            Some((
                app.to_string(),
                target.id_string(),
                name.clone(),
                tilo_actions::remote::InvokeArgument::Json(arguments),
            ))
        }
        Invocation::Action { .. } => None,
        Invocation::Edit {
            app,
            target,
            field,
            value,
            ..
        } => {
            let action = match value {
                Value::Bool(_) => tilolang_tilo::action::EDIT_FLAG_ACTION,
                _ => tilolang_tilo::action::EDIT_TEXT_ACTION,
            };
            let value = tilolang_tilo::runtime::value::to_argument(value).unwrap_or_default();
            let arguments = serde_json::Map::from_iter([
                // The field as its type names it, which the API's edit takes.
                ("field".to_owned(), field.clone().into()),
                ("value".to_owned(), value),
            ]);
            Some((
                app.to_string(),
                target.id_string(),
                action.to_owned(),
                tilo_actions::remote::InvokeArgument::Json(arguments),
            ))
        }
    }
}

/// Say what another app's api answered of a change asked of it, when it refused or failed:
/// the change never came, and its resource reads as it did.
fn said_back(app: &str, message: &tilo_actions::remote::RemoteActionMessage) {
    use tilo_actions::remote::{ReceiptStatus, RemoteActionMessage};
    if let RemoteActionMessage::Receipt(receipt) = message {
        match &receipt.status {
            ReceiptStatus::Rejected { error } => {
                tracing::warn!(
                    app,
                    ?error,
                    "another app's api refused a change asked of it"
                );
            }
            ReceiptStatus::Failed { error } => {
                tracing::warn!(app, ?error, "a change asked of another app's api failed");
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

fn failed(invocation: tilo_actions::InvocationId, refused: Refused) -> InvocationUpdate {
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
) -> Result<EncodedAction, tilo_actions::ApiError> {
    match argument.and_then(|text| text.strip_prefix(tilo_actions::ARGUMENTS_JSON_PREFIX)) {
        Some(json) => {
            let arguments: serde_json::Map<String, serde_json::Value> = serde_json::from_str(json)
                .map_err(|error| tilo_actions::ApiError::InvalidArguments(error.to_string()))?;
            tilo_actions::api::encode_json(resources, address, &arguments)
        }
        None => tilo_actions::api::encode(resources, address, argument),
    }
}

fn api_code(error: &tilo_actions::ApiError) -> &'static str {
    match error {
        tilo_actions::ApiError::Unknown(_) => "action_unknown",
        tilo_actions::ApiError::Unavailable { .. } => "action_unavailable",
        _ => "action_invalid_arguments",
    }
}

/// What is written while it is held is a step of the run it names ([`ResourceStore::set_run`]).
struct InRun(ResourceStore);

impl InRun {
    fn new(store: &ResourceStore, run: tilolang_tilo::runtime::store::Run) -> Self {
        store.set_run(Some(run));
        Self(store.clone())
    }
}

/// Whether `action` is the one a package declares for the file `file`: the extension its name ends
/// in, which is the narrower claim, or the media type the name declares.
fn declared_for_file(
    action: &tilolang_tilo::tilolang::meta::ActionMeta,
    file: &runtime_portal::process_link::OpenedFile,
) -> bool {
    let media = tilolang_tilo::runtime::blob::media_of(file.name.as_deref().unwrap_or_default());
    action.opens.iter().any(|kind| {
        if kind.contains('/') {
            *kind == media
        } else {
            file.name.as_deref().is_some_and(|name| {
                name.rsplit_once('.')
                    .is_some_and(|(_, extension)| extension.eq_ignore_ascii_case(kind))
            })
        }
    })
}

impl Drop for InRun {
    fn drop(&mut self) {
        self.0.set_run(None);
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

/// Tell the window of `instance` the actions `descriptors` offers: all of them, or, `changed`
/// only, the actions of the resources `changed` names with every resource offered, in order
/// ([`ActionCatalogue::Changed`]).
fn say_catalogue(
    instance: &mut Instance,
    descriptors: &[ActionDescriptor],
    names: impl Fn(&str) -> bool,
    changed: bool,
) {
    let mut resources: Vec<String> = Vec::new();
    let mut actions: Vec<&ActionDescriptor> = Vec::with_capacity(descriptors.len());
    for descriptor in descriptors {
        let resource = descriptor.address.resource.as_str();
        if changed && !resources.iter().rev().any(|held| held == resource) {
            resources.push(resource.to_owned());
        }
        if names(resource) {
            actions.push(descriptor);
        }
    }
    let actions = match serde_json::to_vec(&actions) {
        Ok(actions) => actions,
        Err(error) => {
            tracing::error!(%error, "an action catalogue did not encode");
            return;
        }
    };
    instance.say(ToUi::Actions(if changed {
        ActionCatalogue::Changed { actions, resources }
    } else {
        ActionCatalogue::All(actions)
    }));
    // The window takes it now, not on the frame after.
    instance.flush();
}

/// A content store held in memory: what an instance on this device alone keeps its content in.
#[must_use]
pub fn memory_content() -> tilo_content::ContentStore {
    tilo_content::ContentStore::memory()
}

/// Give each version `history` lists the sentence it reads as in `locale`: what was done, by the
/// label its action is published under in `resources`, to the resource by the name history
/// calls it ("Ran “New note” on Untitled", "Created Notebook"), never by an id.
fn say_versions(history: &mut serde_json::Value, resources: &[ResourceSpec], locale: &str) {
    let mut labels = tilo_history::ActionLabels::default();
    labels.learn(resources);
    let reader = tilo_history::Reader::new(locale).with_labels(&labels);
    let Some(versions) = history
        .get_mut("versions")
        .and_then(serde_json::Value::as_array_mut)
    else {
        return;
    };
    for version in versions {
        let text = |key: &str| version[key].as_str().unwrap_or_default().to_owned();
        let (resource, action, label) = (text("resource"), text("action"), text("label"));
        let app = tilolang_tilo::publish::app_of(&resource)
            .unwrap_or_default()
            .to_owned();
        let entry = tilo_actions::ActionContext {
            app: Some(tilo_actions::ActionApp {
                id: app.clone(),
                label: String::new(),
            }),
            item: Some(tilo_actions::ActionItem {
                id: resource,
                label,
            }),
            what: Some(tilo_actions::Did::Verb(tilo_actions::VerbCall {
                action: tilo_actions::ActionKey {
                    app,
                    kind: String::new(),
                    action,
                },
                args: std::collections::BTreeMap::new(),
            })),
            ..tilo_actions::ActionContext::default()
        };
        let sentence = reader.sentence(&entry);
        if let Some(fields) = version.as_object_mut() {
            fields.insert("sentence".to_owned(), sentence.into());
        }
    }
}
