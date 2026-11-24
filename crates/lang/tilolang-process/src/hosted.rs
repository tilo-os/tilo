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
//!   verbs (Undo, Redo, History, ...: `tilo_resource_store::history::verbs`), `read`
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
use collaboration::{CollabEvent, CollabRequest, SurfaceDelta, SurfacePublication};
use runtime_portal::process_link::Shows;
use tilo_actions::{
    ActionAddress, ActionContext, ActionDescriptor, ActionInvoker, EncodedAction, InvocationUpdate,
    ResourceSpec,
};
use tilo_render_protocol::WireError;
use tilo_render_protocol::patch::{ToUi, Transport};
use tilo_resource_store::history::verbs::{EDIT_TYPE, HistoryEdit, VIEW_TYPE};
use tilolang_tilo::action::{ACTION_TYPE, DynAction};
use tilolang_tilo::background::Waker;
use tilolang_tilo::bundle::Bundle;
use tilolang_tilo::call::Callees;
use tilolang_tilo::host::Host;
use tilolang_tilo::instance::{Config, Instance, LaterBuilds, Locale, Mode, Platform, Publication};
use tilolang_tilo::publish::HOST_RESOURCE;
use tilolang_tilo::publish::{Caller, Moved, Planner, Publications, publish_package};
use tilolang_tilo::resources::ResourceStore;
use tilolang_tilo::runtime::delegate::{Invocation, Invoked, Invoker};
use tilolang_tilo::runtime::ids::MemberId;
use tilolang_tilo::runtime::{InvokeOutcome, RunOutcome, Value};
use tilolang_tilo::used::UsedApp;
use tilolang_tilo::{HostEffect, OpenEffect};

mod used;
use tilolang_tilo::platform_apps::Roots;
use used::{Used, used_apps};

use crate::call::Calls;
use crate::invocations::Invocations;
use crate::shell_page::ShellPage;
/// The package whose `Playback` an app's session keeps: the one a window's playback outputs come
/// from.
const PLAYBACK_APP: &str = "so.tilo.playback";

use crate::phrases::Phrases;
use crate::presence::Presence;

/// The API Tester's app id: the one app the general network client answers its requests for.
const API_TESTER_APP: &str = "so.tilo.api-tester";

thread_local! {
    /// The caller of the invocation being built on this thread, as the broker delivered it.
    static CALLER: RefCell<Option<Caller>> = const { RefCell::new(None) };
    /// The invocation the broker is serving on this thread, as the app's code runs for it: the
    /// calls that code makes are its children.
    static SERVING: RefCell<Option<Serving>> = const { RefCell::new(None) };
}

/// The invocation the broker served, whose code runs on this thread: the calls it makes continue
/// it, decided as its caller's and scoped to its authority.
#[derive(Clone, Debug)]
struct Serving {
    invocation: tilo_actions::InvocationId,
    /// The person the served invocation acts for, when the broker named one.
    person: Option<String>,
    /// The action it names, as the app publishes it; empty for a run that goes on after a call
    /// it waited on.
    action: String,
}

/// What one instance is: the app, the workspace, the window and who is at it, as the session named
/// them for the app's launch.
#[derive(Clone, Debug, PartialEq)]
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
    /// Where the workspace's resources open on the web, as the session handed it
    /// (`InstanceRequest::link_home`): what `share-link` mints under, and nothing where the
    /// workspace's host serves no page.
    pub link_home: Option<tilolang_tilo::share::LinkHome>,
    /// What the providers the device registered publish at fixed addresses
    /// (`InstanceRequest::provided`): a package the app uses that is one of them is called at
    /// the addresses its provider registered, through the dispatch as any app's call is.
    pub provided: Vec<tilo_actions::remote::ProvidedPackage>,
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
            link_home: None,
            provided: Vec::new(),
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

/// The documents the package holds in `record`'s workspace, each by the id it holds: its records,
/// which what an earlier native app of that id kept are among, and which the package's migrations
/// carry over on the device whose turn each one is.
///
/// Read through the instance's connection, as the device's own roles read its workspace's
/// documents, and never written: the host keeps no live copy of a document. The app's documents
/// are listed first and then read by id, a document that did not fit one answer asked for again.
/// While one of them is on its way, none are handed over: carrying some of them would leave the
/// rest behind for good, so the carrying waits for a start that reads them all.
fn earlier_documents(record: &Connection, app: &str) -> Vec<(String, automerge::AutoCommit)> {
    // Only a connection to a workspace can read its documents: one with no broker answers none.
    if record.is_local() {
        return Vec::new();
    }
    read_earlier(&record.port(None), app)
}

/// The records `port` holds of `app`, each whole and loaded, by the id it holds: listed first and
/// then read by id, as every port that sends the workspace record commands reads them
/// ([`collaboration::resource::commands`]). While one of them is on its way, none are returned:
/// the carrying waits for a start that reads them all.
fn read_earlier(
    port: &dyn tilo_resource_store::Port,
    app: &str,
) -> Vec<(String, automerge::AutoCommit)> {
    use collaboration::resource::{DocumentAsk, DocumentRead, commands};
    let ask = |command| port.command(command);
    let listed = match commands::list(&ask) {
        Ok(listed) => listed,
        Err(error) => {
            tracing::warn!(app, %error, "the app's records do not list; carrying waits");
            return Vec::new();
        }
    };
    let mut read = Vec::with_capacity(listed.len());
    for (key, _) in listed {
        let wanted = DocumentAsk {
            id: key.clone(),
            since: None,
        };
        let bytes = match commands::read(&ask, &wanted) {
            Ok(DocumentRead::Whole { bytes, .. }) => bytes,
            Ok(DocumentRead::Absent) => continue,
            Ok(other) => {
                tracing::warn!(
                    app,
                    key,
                    ?other,
                    "a record is not here whole; carrying waits"
                );
                return Vec::new();
            }
            Err(error) => {
                tracing::warn!(app, key, %error, "a record does not read; carrying waits");
                return Vec::new();
            }
        };
        match automerge::AutoCommit::load(&bytes) {
            Ok(doc) => {
                let id = collab_types::document::identity::identity(&doc)
                    .map_or(key, |held| held.id.as_str().to_owned());
                read.push((id, doc));
            }
            Err(error) => {
                tracing::warn!(app, key, %error, "a record does not open; carrying it waits");
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
                .addressed(&self.host, &self.planner, address)
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
    let mut resource = tilolang_service::with_history(resource);
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
    /// from. `None` on a session that hands none, and then this window keeps what it is given
    /// in [`Self::kept`] instead.
    content: Option<tilo_content::ContentStore>,
    /// Where a file a person chooses is kept when the session handed no store: this window's
    /// own, in memory. It lasts as long as the window, which is what a device that keeps no
    /// store can promise; the person is still asked, and the file still opens.
    own_content: tilo_content::ContentStore,
    /// The person's own chooser, which the session opened for this instance.
    chooser: Option<std::sync::Arc<dyn tilo_dialogs::FilePicker>>,
    /// The person's clipboard, which the session opened for this instance.
    clipboard: Option<std::sync::Arc<tilo_clipboard::service::Connected>>,
    /// The device's preview cache, shared with every other instance of this host and opened the
    /// first time one is asked for.
    thumbnails: Option<std::sync::Arc<std::sync::OnceLock<Arc<thumbnail::ThumbnailService>>>>,
    /// The session's tools service, over the connection the session made for this instance: what
    /// `File.open` asks which app opens a file and starts it.
    tools: Option<std::sync::Arc<runtime_portal::tools::Service>>,
    /// What a person asking for a file is told when the session opened no chooser.
    no_chooser: String,
    /// The member the instance acts for: whose changes a watched folder's action makes.
    person: String,
    /// The folders this instance watches, shared with the frames that run their actions.
    watches: Rc<crate::files::Watches>,
    /// The platform packages this bundle uses, loaded into its host before the bundle links.
    platform: Rc<RefCell<Roots>>,
    /// What wakes this instance when a followed platform root publishes a reading.
    root_wake: tilo_wake::WakeHandle,
    /// The vault items this window shows the code of, each followed from where the session
    /// publishes it: the code reaches the view as a reading, never as a record or a change.
    vaults: Rc<RefCell<BTreeMap<String, VaultWatch>>>,
    /// The checks the project's host runs for this instance's served invocations.
    checks: Rc<crate::approved_checks::Checks>,
}

/// One vault item this window follows: its reading, and the code last read from it.
struct VaultWatch {
    item: collaboration::SessionResource,
    /// The code last read, kept for the frames between two readings.
    reading: Option<tilolang_tilo::runtime::host_work::VaultCode>,
}

impl HostPlatform {
    /// The content store this window keeps files in: the device's for the app in its workspace,
    /// which the session opened for it, else the one the window keeps itself, so a file a person
    /// chooses still opens where no store was handed.
    fn store(&self) -> &tilo_content::ContentStore {
        self.content.as_ref().unwrap_or(&self.own_content)
    }

    /// The device's preview cache, opened the first time a preview is asked for; `None` on a host
    /// that keeps no previews.
    fn previews(&self) -> Option<Arc<thumbnail::ThumbnailService>> {
        let shared = self.thumbnails.as_ref()?;
        Some(Arc::clone(shared.get_or_init(|| {
            Arc::new(
                thumbnail::ThumbnailService::open_default(1)
                    .unwrap_or_else(|_| thumbnail::ThumbnailService::ephemeral(1)),
            )
        })))
    }
}

/// One `tilolang-process` hosts multiple windows of different apps on a single thread — some
/// shipping the person's language, some English-only — and receives region formats over the
/// session API rather than reading toolkit thread-locals (this crate depends on `tilo-ui` for
/// presence decoding and agent task sessions, not on `tilo-app-window`). Each window therefore
/// keeps its own [`Phrases`] and reads the session-pushed [`Formats`] cell.
///
/// `words_revision` moves when the session pushes a language change (`Hosted::speak`) or a
/// package or later build adds catalogues (`Phrases::add_package`). Nothing in `tilolang-process`
/// reads on-disk language packs: its chrome and toolkit catalogues are compiled into the binary
/// and change only with a new build.
impl Locale for HostPlatform {
    fn add_words(&self, _window: bool, catalogues: &[(String, String)]) {
        // The window's language was already chosen in `Phrases::for_window` from the package's
        // shipped locales and the session's `Spec::language`.
        self.phrases.add_package(catalogues);
    }

    fn words_revision(&self) -> u64 {
        self.phrases.revision()
    }

    fn language(&self) -> String {
        self.phrases.language()
    }

    fn formats(&self) -> tilo_formats::RegionFormats {
        self.formats.get()
    }

    fn format_words(&self) -> &dyn tilo_formats::Words {
        &*self.phrases
    }
}

/// This device's content store, as the Passwords engine asks for a file's bytes and keeps what it
/// makes: the file address an app holds names the content, never a path.
#[cfg(all(
    feature = "app-launch",
    any(target_os = "linux", all(target_os = "macos", target_arch = "aarch64"))
))]
struct EngineContent<'a>(&'a HostPlatform);

#[cfg(all(
    feature = "app-launch",
    any(target_os = "linux", all(target_os = "macos", target_arch = "aarch64"))
))]
impl passwords_engine::Device for EngineContent<'_> {
    fn read(&self, hash: &str) -> Option<Vec<u8>> {
        self.0.read_content(hash)
    }

    fn keep(&self, name: &str, media: &str, bytes: &[u8]) -> Result<String, String> {
        self.0
            .keep_content(name, media, bytes)
            .map(|file| file.hash)
    }
}

/// What a stream running beside the host hands over as frames arrive: each is published where the
/// window paints live pictures from, kept for the embedder that reads them, and the window is woken
/// to draw it. Never polled.
struct StreamFrames {
    push: Arc<dyn tilolang_tilo::runtime::host_work::Push>,
    wake: Waker,
}

/// The frames of `stream`, handed to a loaded engine that makes them.
fn frames_of(stream: &Arc<StreamFrames>) -> tilo_engine_loader::Frames {
    let context = Arc::as_ptr(stream).cast_mut().cast::<std::ffi::c_void>();
    tilo_engine_loader::Frames::new(context, pushed)
}

/// One frame a loaded engine made, shown by the window the stream belongs to.
fn pushed(
    context: *mut std::ffi::c_void,
    slot: &str,
    generation: u64,
    width: u32,
    height: u32,
    pixels: &[u8],
) {
    // SAFETY: the context is the `StreamFrames` the stream's caller kept for the process's life.
    let frames = unsafe { &*context.cast::<StreamFrames>() };
    tilolang_tilo::runtime::host_work::Push::frame(
        frames,
        tilolang_tilo::runtime::host_work::Frame {
            slot: slot.to_owned(),
            generation,
            width,
            height,
            pixels: Arc::new(pixels.to_vec()),
        },
    );
}

/// The streams loaded engines run, by the job's id: what their frames reach while they run.
fn streams() -> &'static std::sync::Mutex<std::collections::BTreeMap<String, Arc<StreamFrames>>> {
    static STREAMS: std::sync::OnceLock<
        std::sync::Mutex<std::collections::BTreeMap<String, Arc<StreamFrames>>>,
    > = std::sync::OnceLock::new();
    STREAMS.get_or_init(|| std::sync::Mutex::new(std::collections::BTreeMap::new()))
}

impl tilolang_tilo::runtime::host_work::Push for StreamFrames {
    fn frame(&self, frame: tilolang_tilo::runtime::host_work::Frame) {
        ui_core::live_pictures::publish(
            &frame.slot,
            frame.generation,
            frame.width,
            frame.height,
            Arc::clone(&frame.pixels),
        );
        self.push.frame(frame);
        (self.wake)();
    }

    fn state(&self, slot: &str, state: &str) {
        self.push.state(slot, state);
        (self.wake)();
    }

    fn ended(&self, slot: &str, state: &str) {
        self.push.ended(slot, state);
        (self.wake)();
    }
}

impl Platform for HostPlatform {
    /// The latest later build when there is one that loads, else the package the instance was
    /// started with.
    fn load(&self, host: &Host) -> u64 {
        // The platform's packages are descriptions the bundle's `use`s name: loaded before the
        // bundle, whose types read them.
        self.platform
            .borrow_mut()
            .load(host, &self.bundle.meta.uses, &self.root_wake);
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

    /// The code of this moment from the vault item `reference` names: followed from where the
    /// session publishes the item's reading, computed where the seed is. Nothing is written down,
    /// and the reading wakes the window so the view draws the new code.
    fn vault_code(&self, reference: &str) -> Option<tilolang_tilo::runtime::host_work::VaultCode> {
        let mut vaults = self.vaults.borrow_mut();
        if !vaults.contains_key(reference) {
            let item =
                match collaboration::SessionResource::follow(reference, self.root_wake.clone()) {
                    Ok(item) => item,
                    Err(error) => {
                        tracing::debug!(%reference, %error, "the vault item is not followed");
                        return None;
                    }
                };
            vaults.insert(
                reference.to_owned(),
                VaultWatch {
                    item,
                    reading: None,
                },
            );
        }
        let watch = vaults.get_mut(reference)?;
        if let Some(Ok(text)) = watch.item.latest()
            && let Some(code) = vault_reading(&text)
        {
            watch.reading = Some(code);
        }
        watch.reading.clone()
    }

    /// Receivers decide each change against the surface of the build that runs now.
    fn surface_changed(&self, app: &str, bundle: &Bundle) {
        match bundle.surface() {
            Ok(surface) => collab_types::app_surface::register_package(app, &surface),
            // The surface registered before stays: receivers keep deciding against it.
            Err(error) => tracing::error!(app, %error, "the later build's surface is unusable"),
        }
    }

    fn locale(&self) -> Option<&dyn Locale> {
        Some(self)
    }

    fn phrase(&self, id: &str, args: &[(String, String)]) -> String {
        self.phrases.say(id, args)
    }

    fn now(&self) -> String {
        tilolang_tilo::instance::wall_now()
    }

    fn moment(&self) -> tilolang_tilo::instance::Zoned {
        tilolang_tilo::instance::wall_moment()
    }

    fn web(
        &self,
        request: &tilolang_tilo::runtime::run::WebRequest,
    ) -> tilolang_tilo::runtime::web::WebReply {
        crate::web::fetch(request)
    }

    fn read_content(&self, hash: &str) -> Option<Vec<u8>> {
        crate::files::read(self.store(), hash)
    }
    /// The device's PDF engine: the one reader crate over the one PDF engine, answering every
    /// `pdf.*` call from the bytes the runtime read out of the content store.
    fn pdf(&self) -> Option<std::rc::Rc<dyn tilolang_tilo::runtime::pdf::PdfHost>> {
        Some(crate::pdf::Engine::host())
    }

    /// The device's drawing engine: `tilo-canvas-engine` over the artwork's records, answering
    /// every `canvas.*` call from the text and picture bytes the runtime read.
    fn canvas(&self) -> Option<std::rc::Rc<dyn tilolang_tilo::runtime::canvas::CanvasHost>> {
        Some(crate::canvas::Engine::host())
    }

    /// The engines this device links; every other app's asks go to the session's own registry.
    /// The Passwords, Video Editor, Feeds, Print Centre and API Tester engines build for Linux and
    /// Apple silicon alone.
    #[cfg(feature = "app-launch")]
    fn engine_call(&self, app: &str, op: &str, input: &str) -> Result<String, String> {
        // The engines this process opens from its runtime bundle rather than linking: an app's
        // typesetting, its document checks, its text merges, and the ones that follow them.
        if crate::loaded_engines::answers(app) {
            return crate::loaded_engines::call(self, app, op, input);
        }
        #[cfg(any(target_os = "linux", all(target_os = "macos", target_arch = "aarch64")))]
        {
            if app == passwords_engine::APP {
                let device = EngineContent(self);
                if let Some(answer) = crate::passwords_vault::call(&device, op, input) {
                    return answer;
                }
                return passwords_engine::call(&device, app, op, input);
            }
            if app == crate::video_engine::APP {
                return crate::video_engine::call(self, app, op, input);
            }
            if app == crate::print_centre_engine::APP {
                return crate::print_centre_engine::call(self, app, op, input);
            }
            crate::backups_engine::register();
            if app == API_TESTER_APP {
                return tilo_network_client::call(app, op, input);
            }
            crate::feed_engine::register();
        }
        if app == crate::containers_engine::APP {
            return crate::containers_engine::call(self, app, op, input);
        }
        if app == crate::git_engine::APP {
            return crate::git_engine::call(app, op, input);
        }
        // YouTube's Grayjay link, whose exchanges touch the network and whose key is the app's
        // own secret.
        crate::grayjay_engine::register();
        tilolang_tilo::engine::answer(app, op, input)
    }

    #[cfg(feature = "app-launch")]
    fn engine_start(&self, app: &str, op: &str, input: &str) -> Result<String, String> {
        if crate::loaded_engines::answers(app) {
            return crate::loaded_engines::start(self, app, op, input);
        }
        #[cfg(any(target_os = "linux", all(target_os = "macos", target_arch = "aarch64")))]
        {
            if app == API_TESTER_APP {
                return tilo_network_client::start(app, op, input);
            }
            if app == crate::video_engine::APP {
                return crate::video_engine::start(self, app, op, input);
            }
            if app == crate::print_centre_engine::APP {
                return crate::print_centre_engine::start(self, app, op, input);
            }
            crate::backups_engine::register();
            crate::feed_engine::register();
        }
        if app == crate::containers_engine::APP {
            return crate::containers_engine::start(self, app, op, input);
        }
        if app == crate::git_engine::APP {
            return crate::git_engine::start(app, op, input);
        }
        crate::grayjay_engine::register();
        tilolang_tilo::engine::begin(app, op, input)
    }

    /// A job whose record it pushes as it moves and when it ends: Photos' own reads the pictures
    /// a person chose; any other app's engine starts as [`Self::engine_start`] does, or pushes its
    /// record when it is one registered for its app.
    #[cfg(feature = "app-launch")]
    fn engine_start_held(
        &self,
        app: &str,
        op: &str,
        input: &str,
        push: Arc<dyn tilolang_tilo::runtime::host_work::Push>,
    ) -> Result<String, String> {
        let record = Arc::new(StreamFrames {
            push,
            wake: self.waker(),
        });
        if crate::loaded_engines::answers(app) {
            return crate::loaded_engines::start(self, app, op, input);
        }
        if app == crate::photos_engine::APP {
            return crate::photos_engine::start(
                self.store().clone(),
                self.previews(),
                op,
                input,
                record,
            );
        }
        // The engines this host answers itself start their jobs as they always have.
        #[cfg(any(target_os = "linux", all(target_os = "macos", target_arch = "aarch64")))]
        {
            let routed = [
                crate::video_engine::APP,
                crate::print_centre_engine::APP,
                API_TESTER_APP,
            ];
            if routed.contains(&app) {
                return self.engine_start(app, op, input);
            }
            crate::backups_engine::register();
            crate::feed_engine::register();
        }
        if app == crate::containers_engine::APP || app == crate::git_engine::APP {
            return self.engine_start(app, op, input);
        }
        tilolang_tilo::engine::begin_held(app, op, input, record)
    }

    #[cfg(feature = "app-launch")]
    fn engine_stream(
        &self,
        app: &str,
        op: &str,
        input: &str,
        push: Arc<dyn tilolang_tilo::runtime::host_work::Push>,
    ) -> Result<String, String> {
        let frames = Arc::new(StreamFrames {
            push,
            wake: self.waker(),
        });
        if crate::loaded_engines::answers(app) {
            let answer = crate::loaded_engines::stream(self, app, op, input, &frames_of(&frames))?;
            // Kept for the process's life, as the engine's own job table is: the thread the
            // engine started may hand one last frame after the app stops showing it.
            streams()
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(answer.clone(), Arc::clone(&frames));
            return Ok(answer);
        }
        tilolang_tilo::engine::stream(app, op, input, frames)
    }

    /// A check the project's host approved, for the invocation the broker is serving on this
    /// thread, on the route the instance joined: the same queue a Rust app's check goes through.
    fn approved_check(&self, _app: &str, check_id: &str) -> Result<String, String> {
        let served = SERVING.with(|served| served.borrow().clone());
        self.checks.start(
            served
                .as_ref()
                .map(|served| crate::approved_checks::Served {
                    invocation: served.invocation,
                    action: &served.action,
                }),
            check_id,
        )
    }

    #[cfg(not(feature = "app-launch"))]
    fn engine_status(&self, job: &str) -> Result<String, String> {
        match self.checks.status(job) {
            Some(status) => Ok(status),
            None => tilolang_tilo::engine::progress(job),
        }
    }

    #[cfg(not(feature = "app-launch"))]
    fn engine_cancel(&self, job: &str) -> bool {
        self.checks.cancel(job) || tilolang_tilo::engine::stop(job)
    }

    #[cfg(feature = "app-launch")]
    fn engine_status(&self, job: &str) -> Result<String, String> {
        if let Some(status) = self.checks.status(job) {
            return Ok(status);
        }
        if let Some(status) = crate::loaded_engines::status(self, job) {
            return status;
        }
        #[cfg(any(target_os = "linux", all(target_os = "macos", target_arch = "aarch64")))]
        {
            crate::backups_engine::register();
            crate::feed_engine::register();
            if let Ok(status) = tilo_network_client::status(job)
                .or_else(|_| crate::video_engine::status(self, job))
                .or_else(|_| crate::print_centre_engine::status(self, job))
            {
                return Ok(status);
            }
        }
        crate::git_engine::status(job).or_else(|unknown| {
            crate::containers_engine::status(job)
                .or_else(|_| tilolang_tilo::engine::progress(job).map_err(|_| unknown))
        })
    }

    #[cfg(feature = "app-launch")]
    fn engine_cancel(&self, job: &str) -> bool {
        if self.checks.cancel(job) {
            return true;
        }
        if let Some(cancelled) = crate::loaded_engines::cancel(self, job) {
            return cancelled;
        }
        #[cfg(any(target_os = "linux", all(target_os = "macos", target_arch = "aarch64")))]
        {
            crate::backups_engine::register();
            crate::feed_engine::register();
            if tilo_network_client::cancel(job)
                || crate::video_engine::cancel(job)
                || crate::print_centre_engine::cancel(job)
            {
                return true;
            }
        }
        // A stream's last frame goes with its job: the window shows the session closed, not a
        // frozen screen.
        ui_core::live_pictures::forget(job);
        crate::git_engine::cancel(job)
            || crate::containers_engine::cancel(job)
            || crate::photos_engine::cancel(job)
            || tilolang_tilo::engine::stop(job)
    }

    fn choose_files(
        &self,
        request: &tilolang_tilo::runtime::blob::FileRequest,
    ) -> Result<Vec<tilolang_tilo::runtime::blob::FileValue>, String> {
        // The person is asked whether or not this session handed the app a store: a session that
        // serves no store still serves the chooser, and the file it is given opens here.
        let Some(chooser) = self.chooser.as_deref() else {
            return Err(self.no_chooser.clone());
        };
        crate::files::choose(self.store(), chooser, request)
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
        crate::files::keep(self.store(), name, media, bytes)
    }

    /// `File.thumbnail(size)`: a preview no larger than `size` pixels a side, from the device's
    /// shared cache; `None` for a file that has no preview.
    fn thumbnail(
        &self,
        file: &tilolang_tilo::runtime::blob::FileValue,
        size: u32,
    ) -> Result<Option<tilolang_tilo::runtime::blob::FileValue>, String> {
        let Some(thumbnails) = self.previews() else {
            return Err("this host keeps no previews".to_owned());
        };
        crate::files::thumbnail(self.store(), &thumbnails, file, size)
    }

    /// `clipboard.copy` and `clipboard.copy_image`: the person's copy, offered on the selection
    /// the session holds, as any app's copy is.
    fn copy_clipboard(&self, mime: &str, bytes: &[u8]) -> bool {
        self.clipboard
            .as_ref()
            .is_some_and(|clipboard| clipboard.copy(mime, bytes))
    }

    /// `clipboard.copy_secret`: a secret copy on the session's selection, marked so histories and
    /// device sync do not keep it, and taken off again once it has done its job.
    fn copy_secret_clipboard(&self, text: &str, clear_after: std::time::Duration) -> bool {
        self.clipboard
            .as_ref()
            .is_some_and(|clipboard| clipboard.copy_secret(text, clear_after))
    }

    /// `clipboard.paste` and `clipboard.paste_image`: what the person's selection holds.
    fn paste_clipboard(&self, types: &[&str]) -> Option<(String, Vec<u8>)> {
        self.clipboard
            .as_ref()
            .and_then(|clipboard| clipboard.paste(types))
    }

    fn list_files(&self, path: &str) -> Result<Vec<tilolang_tilo::runtime::files::Entry>, String> {
        crate::files::list(std::path::Path::new(path))
    }

    fn stat_file(
        &self,
        path: &str,
    ) -> Result<Option<tilolang_tilo::runtime::files::Entry>, String> {
        crate::files::stat(std::path::Path::new(path))
    }

    fn read_file_bytes(&self, path: &str) -> Result<Vec<u8>, String> {
        crate::files::read_bytes(std::path::Path::new(path))
    }

    fn watch_folder(&self, path: &str, run: &str) -> Result<String, String> {
        self.watches.watch(path, run, &self.person)
    }

    fn unwatch_folder(&self, token: &str) {
        self.watches.unwatch(token);
    }

    fn make_folder(&self, path: &str) -> Result<tilolang_tilo::runtime::files::Entry, String> {
        crate::files::mkdir(path)
    }

    fn compress_files(&self, paths: &[String], to: &str) -> Result<i64, String> {
        crate::files::compress(paths, to)
    }

    /// `tool.document_apps`: the shell's own decision, by the file's first bytes and its name.
    fn document_apps(
        &self,
        path: &str,
    ) -> Result<Vec<tilolang_tilo::runtime::files::DocumentApp>, String> {
        let Some(tools) = self.tools.as_deref() else {
            return Err("the shell is not answering which app opens a file".to_owned());
        };
        tools.list_document_apps(path).map(|apps| {
            apps.into_iter()
                .map(|app| tilolang_tilo::runtime::files::DocumentApp {
                    id: app.id,
                    name: app.name,
                    is_default: app.is_default,
                    icon: app.icon.unwrap_or_default(),
                })
                .collect()
        })
    }

    fn open_with_app(&self, path: &str, app: &str) -> Result<(), String> {
        let Some(tools) = self.tools.as_deref() else {
            return Err("the shell is not answering which app opens a file".to_owned());
        };
        tools.open_document(path, Some(app))
    }

    fn open_terminal_at(&self, folder: &str) -> Result<(), String> {
        let Some(tools) = self.tools.as_deref() else {
            return Err("the shell is not answering which app opens a file".to_owned());
        };
        tools
            .ask(runtime_portal::tools::ToolIntent::Terminal {
                directory: Some(folder.to_owned()),
            })
            .map(drop)
    }

    /// `tool.open_link`: the shell opens the address with the app that opens links of its
    /// scheme, as a person pressing a link does.
    fn open_link(&self, url: &str) -> Result<(), String> {
        let Some(tools) = self.tools.as_deref() else {
            return Err("the shell is not answering which app opens a link".to_owned());
        };
        tools.open_link(url)
    }

    /// What the shell opened this app with: the launcher hands it the tool context it was
    /// started for (`TILO_TOOL_CONTEXT`), and an app opened on its own has none.
    fn launch_context(&self) -> String {
        const MAX_BYTES: usize = 16 * 1024;
        let Ok(context) = std::env::var(runtime_portal::tools::TOOL_CONTEXT_ENV) else {
            return String::new();
        };
        if context.len() > MAX_BYTES {
            return String::new();
        }
        match serde_json::from_str::<serde_json::Value>(&context) {
            Ok(_) => context,
            Err(error) => {
                tracing::warn!(%error, "the tool context the app was started with is not JSON");
                String::new()
            }
        }
    }

    fn open_container_shell(&self, container: &str, engine: &str) -> Result<(), String> {
        let Some(tools) = self.tools.as_deref() else {
            return Err("the shell is not answering a terminal".to_owned());
        };
        let engine = match engine {
            "distrobox" => runtime_portal::tools::ContainerEngine::Distrobox,
            "podman" => runtime_portal::tools::ContainerEngine::Podman,
            other => return Err(format!("no `{other}` runs a container shell")),
        };
        tools
            .ask(runtime_portal::tools::ToolIntent::ContainerShell {
                container: container.to_owned(),
                engine,
            })
            .map(drop)
    }

    fn extract_file(&self, from: &str, to: &str) -> Result<i64, String> {
        crate::files::extract(from, to)
    }

    fn write_file(
        &self,
        path: &str,
        bytes: &[u8],
    ) -> Result<tilolang_tilo::runtime::files::Entry, String> {
        crate::files::write(path, bytes)
    }

    fn rename_file(
        &self,
        path: &str,
        name: &str,
    ) -> Result<tilolang_tilo::runtime::files::Entry, String> {
        crate::files::rename(path, name)
    }

    fn move_file(
        &self,
        path: &str,
        folder: &str,
    ) -> Result<tilolang_tilo::runtime::files::Entry, String> {
        crate::files::move_entry(path, folder)
    }

    fn copy_file(
        &self,
        path: &str,
        folder: &str,
    ) -> Result<tilolang_tilo::runtime::files::Entry, String> {
        crate::files::copy(path, folder)
    }

    fn remove_file(&self, path: &str) -> Result<bool, String> {
        crate::files::remove(path)
    }

    fn trash_file(&self, path: &str) -> Result<tilolang_tilo::runtime::files::TrashEntry, String> {
        crate::files::trash(path)
    }

    fn trash_entries(&self) -> Result<Vec<tilolang_tilo::runtime::files::TrashEntry>, String> {
        crate::files::trash_entries()
    }

    fn restore_file(&self, name: &str) -> Result<tilolang_tilo::runtime::files::Entry, String> {
        crate::files::restore(name)
    }

    /// `File.open`: the shell answers which app opens the file's kind and starts it. A file an
    /// action holds is read here first, since the decision is made from its bytes and its name;
    /// the session then opens it as content the app that opens it reads, never as a path this
    /// sandbox has not.
    fn open_file(
        &self,
        file: &tilolang_tilo::runtime::files::FileTarget<'_>,
    ) -> Result<(), String> {
        let Some(tools) = self.tools.as_deref() else {
            return Err("the shell is not answering which app opens a file".to_owned());
        };
        match file {
            tilolang_tilo::runtime::files::FileTarget::Path(path) => {
                tools.open_document(path, None)
            }
            tilolang_tilo::runtime::files::FileTarget::Content { name, hash } => {
                let bytes = self
                    .read_content(hash)
                    .ok_or_else(|| format!("the content of `{name}` is not on this device"))?;
                let apps = |kind: &str| tools.list_document_apps(kind);
                let app = runtime_portal::tools::document_app_for_with(&bytes, name, &apps)?;
                let Some(app) = app else {
                    return Err(format!("nothing on this computer opens `{name}`"));
                };
                tools.open_content(name, hash, &app.id)
            }
        }
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

/// What an instance is handed beside its store: the files the session opened for it.
#[derive(Default)]
pub struct Kept {
    /// The device content store of the app in its workspace, when the session opened one for
    /// this instance: the files a person chooses and the pictures its resources hold are kept
    /// there, outliving the window.
    pub content: Option<tilo_content::ContentStore>,
    /// The person's chooser, when the session opened one for this instance.
    pub chooser: Option<std::sync::Arc<dyn tilo_dialogs::FilePicker>>,
    /// The person's clipboard, when the session opened one for this instance.
    pub clipboard: Option<std::sync::Arc<tilo_clipboard::service::Connected>>,
    /// The device's preview cache, shared with every instance of this host and opened the first
    /// time one is asked for.
    pub thumbnails: Option<std::sync::Arc<std::sync::OnceLock<Arc<thumbnail::ThumbnailService>>>>,
    /// The session's tools service, over the connection it made for this instance: what
    /// `File.open` asks which app opens a file and starts it.
    pub tools: Option<std::sync::Arc<runtime_portal::tools::Service>>,
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

/// What the system's media controls were last told this instance plays: told nothing, told it
/// plays nothing, or told what it plays.
#[derive(Default)]
enum MediaSaid {
    /// The controls have not been told yet.
    #[default]
    Untold,
    /// The controls were told the instance plays nothing.
    Nothing,
    /// The controls were told what the instance plays.
    Playing(runtime_portal::process_link::MediaView),
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
    /// How a call of another app's action is built, from the packages this app may use.
    callees: Rc<Callees>,
    /// The child calls sent, by the invocation each went out under: what each answers as.
    waiting_calls: Rc<RefCell<BTreeMap<String, Invocation>>>,
    /// The child calls a run waits on, by the invocation each went out under.
    children: Rc<RefCell<BTreeMap<String, Child>>>,
    /// Whether the runtime sends its calls of other apps' actions on this instance's connection.
    sending: bool,
    platform: Rc<RefCell<Roots>>,
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
    /// Who is in the system's calls the views show, and the steps the person takes in them.
    calls: Calls,
    /// The app's calls of its own page actions raised for what the window's page waits on the
    /// person's yes for.
    page_prompts: crate::page_prompts::PagePrompts,
    /// The invocations the views read (an agent's turn, its steps, what it asks), and the lines
    /// the person ran from them.
    invocations: Invocations,
    /// What the shell reads of the device and carries out for its views, where the instance is
    /// the shell shown with no session ([`ShellPage`]).
    shell: Option<ShellPage>,
    /// What the connection delivered before the instance ran, taken on its first frame.
    early: Vec<CollabEvent>,
    /// The records other devices wrote, as the connection told since the last frame: read again
    /// once, after every event of the frame is in, so a burst of writes is one reading.
    moved_records: std::collections::BTreeSet<String>,
    /// What the app was opened with, until the turn that published its surface runs the action its
    /// package declares for the file's kind, or its `open-link` for a link ([`Self::opened`]).
    opening: Option<runtime_portal::process_link::OpenRequest>,
    /// The content store the app's files are kept in, which the session opened for it: the bytes
    /// of the file its launch named are read from here, as its own `file.text()` reads them.
    files: Option<tilo_content::ContentStore>,
    /// The playback's outputs as the window was last told them: sent again only when the app's
    /// `outputs` Collection changed.
    outputs_sent: Option<Vec<u8>>,
    /// What the system's media controls were last told this instance plays: said again only
    /// when it moved.
    media_said: MediaSaid,
    /// The folders this instance watches for `File.watch`, shared with the platform that
    /// registered them: each turn runs the action of a folder that changed.
    watches: Rc<crate::files::Watches>,
    /// The checks the project's host runs for the invocations the broker served, shared with the
    /// platform that starts them.
    checks: Rc<crate::approved_checks::Checks>,
}

/// What invoking one action came to.
enum Ran {
    /// It ran, and this is what it answers.
    Done(tilo_actions::ActionOutput),
    /// It waits on a child call it made: the call's id, and the run that waits on it (`None`
    /// when no run does, the call itself being the served invocation's work).
    Waits {
        call: String,
        run: Option<tilolang_tilo::runtime::ids::RunId>,
    },
}

/// A child call this instance sent, and what waits on its answer.
struct Child {
    /// The run that waits on the answer, when one does: a run of the action the broker served,
    /// or of the action the app was opened with. `None` for a call made outside a run, whose
    /// answer is the served invocation's own.
    run: Option<tilolang_tilo::runtime::ids::RunId>,
    /// The invocation the broker served that the answer ends, when the call was made under one:
    /// the id the broker delivered it under, and the invocation's own id. `None` for a call made
    /// by the app opening a file, which answers nobody.
    served: Option<(u64, tilo_actions::InvocationId)>,
    /// The person the served invocation acts for, when the broker named one: what the run goes
    /// on as when the answer comes.
    person: Option<String>,
    /// The asked record the call waits in, once the dispatch said a person is asked about it:
    /// the served invocation said so once, naming it.
    asked: Option<String>,
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

/// What opening a host makes before the host itself: its instance, and the parts the host keeps.
struct Opened {
    instance: Instance,
    surface: SurfaceSlot,
    builds: BuildsSlot,
    /// What the person may do in the app, which the instance reads at each decision.
    access: Rc<RefCell<Option<tilo_action_policy::MemberAccess>>>,
    /// The platform roots the instance reads the device through, shared with the host.
    roots: Rc<RefCell<Roots>>,
    watches: Rc<crate::files::Watches>,
    used: Vec<Used>,
    personal: PersonalDocument,
    /// The checks the project's host runs for the instance's served invocations.
    checks: Rc<crate::approved_checks::Checks>,
}

/// One invocation the broker delivered: its id, the invocation's own id, the resource and action
/// it names, what it passes, who invokes and the broker's context.
struct Delivered {
    id: u64,
    invocation: tilo_actions::InvocationId,
    resource: String,
    action: String,
    argument: Option<String>,
    actor: tilo_actions::Actor,
    context: Option<Box<tilo_actions::ActionContext>>,
}

/// End the runs an earlier host over `store` left waiting on a child call: the invocation each
/// went out as died with that host, and the change it held went with it, so each is ended with
/// the failure that is true of it rather than left waiting for ever.
fn abandon_open_runs(
    store: &ResourceStore,
    connection: &Connection,
    instance: &Instance,
    app: &str,
) {
    for (call, run, action) in store.open_runs() {
        let reason = format!("`{action}` stopped when its app restarted");
        tracing::warn!(%app, %call, %run, %reason);
        let Some(host) = instance.host() else {
            continue;
        };
        match host.runtime().abandon_run(&run, &reason) {
            Ok(Some((request, invocation))) => {
                // The person sees it where the invocation shows: its own record says it ended,
                // with why.
                let Ok(invocation) = invocation.parse() else {
                    continue;
                };
                let update = failed(invocation, Refused::new("action_failed", reason.clone()));
                if let Err(error) = connection
                    .manager()
                    .try_send(CollabRequest::UpdateInvocation {
                        id: request,
                        update,
                    })
                {
                    tracing::warn!(%app, %run, %error, "the ended invocation was not answered");
                }
            }
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(%app, %run, %error, "a waiting run could not be ended");
            }
        }
    }
}

/// The instance `spec` runs in, on the device services `kept` holds, its words read by `phrases`
/// and written in `formats`: its platform, its earlier documents and its access set, inside the
/// document's scope.
fn open_instance(
    spec: &Spec,
    bundle: &Arc<Bundle>,
    (phrases, formats): (&Rc<Phrases>, &Formats),
    kept: &Kept,
    store: &ResourceStore,
    window: Box<dyn Transport>,
    (wake, scope): (&Waker, &DocumentScope),
    connection: &Connection,
) -> Opened {
    let surface: SurfaceSlot = Rc::default();
    let builds: BuildsSlot = Rc::default();
    let in_scope = scope.enter();
    let used = used_apps(spec, bundle, store);
    let personal = PersonalDocument::open(&spec.app, &spec.person, store);
    let watches = Rc::new(crate::files::Watches::new(Arc::clone(wake)));
    let roots = Rc::new(RefCell::new(Roots::default()));
    // A check goes out on the route the instance joined, the one its invocations arrive on; an
    // instance with no broker runs none.
    let checks = Rc::new(crate::approved_checks::Checks::new(
        (!connection.is_local()).then(|| connection.route().channel_id()),
        Arc::clone(wake),
    ));
    let platform = HostPlatform {
        bundle: Arc::clone(bundle),
        installed: spec.installed.clone(),
        shows: spec.shows,
        phrases: Rc::clone(phrases),
        formats: Rc::clone(formats),
        wake: Arc::clone(wake),
        surface: Rc::clone(&surface),
        builds: Rc::clone(&builds),
        content: kept.content.clone(),
        own_content: memory_content(),
        chooser: kept.chooser.clone(),
        clipboard: kept.clipboard.clone(),
        thumbnails: kept.thumbnails.clone(),
        tools: kept.tools.clone(),
        no_chooser: crate::files::NO_CHOOSER.to_owned(),
        person: spec.person.clone(),
        watches: Rc::clone(&watches),
        platform: Rc::clone(&roots),
        root_wake: scope.0.handle(),
        vaults: Rc::default(),
        checks: Rc::clone(&checks),
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
            link_home: None,
        },
        Rc::new(platform),
        // People by what the workspace's member list calls them, once it arrived.
        Rc::new(tilolang_tilo::names::Installed),
        window,
    );
    // What a native app of the package's id kept: the package's migrations carry it over on the
    // device whose turn each one is.
    instance.set_earlier(earlier_documents(connection, &spec.app));
    // What its runtime offers and invokes is decided by the person's access as the session
    // says it, read at each decision.
    let access = Rc::new(RefCell::new(spec.access.clone()));
    let read = Rc::clone(&access);
    instance.set_access(Rc::new(move || read.borrow().clone()));
    drop(in_scope);
    Opened {
        instance,
        surface,
        builds,
        access,
        roots,
        watches,
        used,
        personal,
        checks,
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
        // The window speaks the person's language where its package ships it, and the package's
        // source language otherwise: the frames' words, the app's own words and the history's
        // sentences all follow that language, never the one this host's process runs in.
        let phrases = Rc::new(window_phrases(phrases, &spec, &bundle));
        // The names the session sends for the apps installed here read in the language this
        // window speaks, whatever language the workspace's people are named in.
        tilo_resource_store::history::authors::set_names_language(&phrases.locale());
        let scope = DocumentScope::of(content, &wake);
        let Opened {
            mut instance,
            surface,
            builds,
            access,
            roots: platform_roots,
            watches,
            used,
            personal,
            checks,
        } = open_instance(
            &spec,
            &bundle,
            (&phrases, formats),
            kept,
            &store,
            window,
            (&wake, &scope),
            &connection,
        );
        let presence = Presence::new(&spec.app, &spec.person);
        let calls = Calls::new(&spec.person);
        let invocations = Invocations::new(&spec.workspace);
        // The shell, shown where no session runs it (a web page of the workspace's desktop),
        // reads its layout, apps and windows from the device's dispatch.
        let shell = (spec.app == crate::shell_page::SHELL_APP && spec.shows == Shows::View)
            .then(|| ShellPage::new(&spec.workspace, &spec.person, &mut instance));
        // The apps this one may call, each by its package: what a call of another app's action
        // is built from.
        let callees = Rc::new(callees_of(&bundle, &spec.workspace, &spec.provided));
        // A run an earlier host over this store left waiting on a child call cannot be answered:
        // the invocation the call went out as died with that host, and the change the run held
        // went with it. It is ended here with the failure that is true of it, rather than left
        // waiting for ever.
        abandon_open_runs(&store, &connection, &instance, &spec.app);
        let waiting_calls: Rc<RefCell<BTreeMap<String, Invocation>>> = Rc::default();
        let children: Rc<RefCell<BTreeMap<String, Child>>> = Rc::new(RefCell::new(BTreeMap::new()));

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
            callees,
            waiting_calls,
            children,
            sending: false,
            platform: platform_roots,
            identity: None,
            scope,
            personal,
            access,
            presence,
            calls,
            page_prompts: crate::page_prompts::PagePrompts::default(),
            invocations,
            shell,
            early: Vec::new(),
            moved_records: std::collections::BTreeSet::new(),
            opening: None,
            files: kept.content.clone(),
            outputs_sent: None,
            media_said: MediaSaid::Untold,
            watches,
            checks,
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
        self.opening = Some(runtime_portal::process_link::OpenRequest::File(file));
    }

    /// Show what `link` names in the app on the turn that publishes its surface: its `open-link`
    /// runs with the link, as a shell hands a link to the app that claims its scheme, and what it
    /// made opens in the window.
    pub fn open_link(&mut self, link: String) {
        self.opening = Some(runtime_portal::process_link::OpenRequest::Uri(link));
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
        // A watch wakes the host from the platform's own watcher when its folder changes: nothing
        // here waits for it.
        [
            self.instance.wakes_in(),
            self.presence.wakes_in(),
            self.calls.wakes_in(),
            self.invocations.wakes_in(),
            self.shell.as_ref().and_then(ShellPage::wakes_in),
        ]
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
        tilo_resource_store::history::authors::set_names_language(&locale);
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
        // A package's first scenario stands on what its outside services hold, which the
        // instance takes in from these samples through the services' origins before it runs.
        tilolang_mail::sample::register();
        #[cfg(feature = "app-launch")]
        tilolang_youtube::sample::register();
        self.instance.stage(Some(scenario));
    }

    /// Run the action of each watched folder that changed since the last frame, as the member
    /// the app runs for. Whether any ran: a watch is the folder's own change of listing, and the
    /// action it runs reads the folder again for what changed inside it.
    fn run_watches(&self) -> bool {
        let due = self.watches.take_changed();
        if due.is_empty() {
            return false;
        }
        let Some(host) = self.instance.host() else {
            return false;
        };
        let root = match host.root_of(&self.spec.app) {
            Ok(root) => root,
            Err(error) => {
                tracing::warn!(app = self.spec.app, %error, "a watched folder has no root to act on");
                return false;
            }
        };
        let mut ran = false;
        for (run, actor) in due {
            let actor = tilolang_tilo::runtime::Actor::Member(MemberId::person(actor.as_str()));
            match host
                .runtime()
                .invoke_action(&actor, (run.as_str(), vec![Value::resource(&root)]))
            {
                Ok(_) => ran = true,
                Err(error) => {
                    tracing::warn!(app = self.spec.app, %error, "a watched folder's action failed");
                }
            }
        }
        ran
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
        // The runtime's calls of other apps' actions go out on this instance's connection, as
        // children of the invocation the broker is serving: installed once the host that makes
        // them is open.
        if !self.sending
            && let Some(host) = self.instance.host()
        {
            let apis = Apis {
                workspace: self.spec.workspace.clone(),
                callees: Rc::clone(&self.callees),
                calls: Rc::clone(&self.waiting_calls),
                manager: self.connection.shared_manager(),
                platform: Rc::clone(&self.platform),
            };
            match &self.shell {
                Some(shell) => host.runtime().set_foreign_invoker(shell.invoker(apis)),
                None => host.runtime().set_foreign_invoker(apis),
            }
            self.sending = true;
        }
        // What the person picked and saw, as their own document keeps it, before the views run.
        if self.personal.take_moved(&self.store)
            && !self
                .instance
                .bring_personal(self.personal.state(), self.spec.instance)
        {
            self.personal.moved_again();
        }
        // Who else is here and who is writing, before the views that show it run.
        changed |= self.take_events(&mut events);
        // The platform's roots are followed before the views run, so the first frame reads what
        // the session publishes rather than an app's waiting state.
        changed |= self.read_platform();
        changed |= self.instance.update();
        // A watched folder that changed runs its action now, as the person's own step.
        changed |= self.run_watches();
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
        // Where the person is, who is in a call and whether they write there, as the views now
        // stand.
        let connection = &self.connection;
        // The steps the person took in a call, through the system's call service.
        for (target, step) in self.instance.take_call_steps() {
            self.calls.step(&target, step);
        }
        if self.instance.reads_calls() || self.calls.joined() {
            self.calls.say(|request| connection.manager().send(request));
        }
        // What the window's page waits on the person's yes for, raised as the app's calls of its
        // own page actions on the page the window shows, or withdrawn.
        let page = self.instance.front_resource();
        for prompt in self.instance.take_page_prompts() {
            changed |= self.page_prompts.take(
                &prompt,
                (&self.spec.workspace, &self.spec.app, page.as_deref()),
                |request| connection.manager().send(request),
            );
        }
        // The invocations the views read, read again while they run, and the lines the person
        // ran, run as their own steps.
        for (target, line) in self.instance.take_line_runs() {
            self.invocations.run_line(&target, &line);
        }
        self.invocations.follow(&self.instance);
        self.invocations
            .say(|request| connection.manager().send(request));
        if let Some(shell) = &mut self.shell {
            shell.turn(&self.instance);
            shell.say(|request| connection.manager().send(request));
        }
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
        // What the checks the project's host runs for served invocations reported, and how the
        // ones that ended ended, as those invocations' progress and answers.
        for (id, update) in self.checks.owed() {
            self.answer_invocation(id, update);
            changed = true;
        }
        changed |= self.records_moved();
        changed |= self.read_used();

        self.publish();
        self.answer_directory();
        self.publish_search_index();
        changed |= self.opened();
        // Where what plays comes out, after this frame's views ran: a person who moved the
        // session to a speaker or a cast target hears it there from the next frame.
        changed |= self.say_outputs();
        changed
    }

    /// Take in who else is here, who writes and what they run from the frame's `events`, before
    /// the views that show them run. Whether any took one.
    fn take_events(&mut self, events: &mut Vec<CollabEvent>) -> bool {
        let before = events.len();
        events.retain(|event| {
            !self.presence.take(event)
                && !self.calls.take(event)
                && !self.invocations.take(event)
                && !self.shell.as_mut().is_some_and(|shell| shell.take(event))
        });
        let took = events.len() != before;
        self.presence.show(&mut self.instance);
        self.calls.show(&mut self.instance);
        self.invocations.show(&mut self.instance);
        took
    }

    /// Run the action the app was opened with, once the instance has published its surface: the
    /// action the package declares for a file's kind, as a chooser hands one to an action, or its
    /// `open-link` with a link, as a shell hands one to the app that claims its scheme. What the
    /// action made opens in the window, as a press of a primary action's does. Whether anything
    /// was opened.
    /// Tell the window where the app's playback plays, when that changed: the session's
    /// `outputs` Collection, each item with its own id, kind, device, name, volume and whether it
    /// stays in step. An app that plays nothing sends none.
    fn say_outputs(&mut self) -> bool {
        if !self
            .bundle
            .imports
            .iter()
            .any(|import| import.meta.app == PLAYBACK_APP)
        {
            return false;
        }
        let Some(host) = self.instance.host() else {
            return false;
        };
        let Ok(root) = host.root_of(&self.spec.app) else {
            return false;
        };
        let runtime = host.runtime();
        let Ok(Value::Ref(playing)) = runtime.read(&root, "playing") else {
            return false;
        };
        let Ok(Value::List(items)) = runtime.read(&playing.target, "outputs") else {
            return false;
        };
        let text = |output: &tilolang_tilo::runtime::ResourceRef, field: &str| match runtime
            .read(output, field)
        {
            Ok(Value::Text(value)) => value,
            _ => String::new(),
        };
        let mut outputs: Vec<serde_json::Value> = Vec::with_capacity(items.len());
        for item in &items {
            let Value::Ref(output) = item else {
                continue;
            };
            let volume = match runtime.read(&output.target, "volume") {
                Ok(Value::Float(volume)) => volume,
                _ => 1.0,
            };
            let synced = match runtime.read(&output.target, "synced") {
                Ok(Value::Bool(synced)) => synced,
                _ => true,
            };
            outputs.push(serde_json::json!({
                "id": output.target.id_string(),
                "kind": text(&output.target, "kind"),
                "device": text(&output.target, "device"),
                "name": text(&output.target, "name"),
                "volume": volume,
                "synced": synced,
            }));
        }
        let Ok(bytes) = serde_json::to_vec(&outputs) else {
            return false;
        };
        if self.outputs_sent.as_deref() == Some(bytes.as_slice()) {
            return false;
        }
        self.outputs_sent = Some(bytes.clone());
        self.instance.say(ToUi::Outputs(bytes));
        self.instance.flush();
        true
    }

    /// What this instance plays, as the system's media controls show it, when it moved since the
    /// last turn said it: its Playback's reading, or `None` when it plays nothing.
    pub fn media(&mut self) -> Option<Option<runtime_portal::process_link::MediaView>> {
        let view = self.media_view();
        let unchanged = match (&self.media_said, &view) {
            (MediaSaid::Nothing, None) => true,
            (MediaSaid::Playing(said), Some(view)) => said == view,
            _ => false,
        };
        if unchanged {
            return None;
        }
        self.media_said = match &view {
            Some(view) => MediaSaid::Playing(view.clone()),
            None => MediaSaid::Nothing,
        };
        Some(view)
    }

    /// The Playback this instance holds, as the system's controls read it: what it is called,
    /// where it stands, how long it runs and what a person can ask of it.
    fn media_view(&self) -> Option<runtime_portal::process_link::MediaView> {
        if !self
            .bundle
            .imports
            .iter()
            .any(|import| import.meta.app == PLAYBACK_APP)
        {
            return None;
        }
        let host = self.instance.host()?;
        let root = host.root_of(&self.spec.app).ok()?;
        let runtime = host.runtime();
        let Ok(Value::Opt(Some(playing))) = runtime.read(&root, "playing") else {
            return None;
        };
        let Value::Ref(playing) = *playing else {
            return None;
        };
        let at = &playing.target;
        // A Playback that stands still is nothing the controls show: the board leaves it out.
        let state = match runtime.read(at, "state") {
            Ok(Value::Enum(state)) => state.variant,
            _ => return None,
        };
        if state == "stopped" {
            return None;
        }
        let text = |field: &str| match runtime.read(at, field) {
            Ok(Value::Text(text)) => text,
            _ => String::new(),
        };
        let number = |field: &str| match runtime.read(at, field) {
            Ok(Value::Float(value)) => value,
            Ok(Value::Int(value)) => ui_core::num::f64_of_i64(value),
            _ => 0.0,
        };
        let queue = match runtime.read(at, "queue") {
            Ok(Value::List(items)) => items.len(),
            _ => 0,
        };
        let seconds = |value: f64| ui_core::num::i64_of_f64(value.max(0.0) * 1_000_000.0);
        Some(runtime_portal::process_link::MediaView {
            id: at.id_string(),
            identity: self.bundle.meta.title.clone(),
            title: text("title"),
            artist: text("artist"),
            art_url: text("cover"),
            playing: state == "playing",
            position_us: seconds(number("at")),
            length_us: seconds(number("length")),
            volume: number("volume"),
            rate: 1.0,
            can_next: queue > 1,
            can_prev: queue > 1,
        })
    }

    /// Run the media control `command` as this instance's Playback's own action, so every
    /// control the desktop carries out is a real action on the resource.
    ///
    /// # Errors
    ///
    /// Nothing plays here, or the action did not run.
    pub fn run_media_command(
        &mut self,
        command: runtime_portal::process_link::MediaCommand,
    ) -> Result<(), String> {
        let host = self
            .instance
            .host()
            .ok_or_else(|| "the app has not started".to_owned())?;
        let root = host
            .root_of(&self.spec.app)
            .map_err(|error| error.to_string())?;
        let playing = match host.runtime().read(&root, "playing") {
            Ok(Value::Opt(Some(playing))) => match *playing {
                Value::Ref(playing) => (*playing.target).clone(),
                _ => return Err("nothing plays here".to_owned()),
            },
            _ => return Err("nothing plays here".to_owned()),
        };
        let length = match host.runtime().read(&playing, "length") {
            Ok(Value::Float(length)) => length,
            _ => 0.0,
        };
        let (name, args) = media_action(command, &playing, length);
        let action = self
            .bundle
            .imports
            .iter()
            .find(|import| import.meta.app == PLAYBACK_APP)
            .and_then(|import| {
                import
                    .meta
                    .actions
                    .iter()
                    .find(|action| !action.scenario && action.name == name)
            })
            .map(|action| action.id.clone())
            .ok_or_else(|| format!("the player declares no `{name}`"))?;
        let actor = tilolang_tilo::runtime::Actor::Member(MemberId::person(&self.spec.person));
        host.runtime()
            .invoke_action(&actor, (action.as_str(), args))
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    /// Run the action the package declares for the file the app was opened with, once the
    /// instance has published its surface: what a person gets by opening a file in the app, as a
    /// chooser hands one to an action. What the action made opens in the window, as a press of a
    /// primary action's does. Whether a file was opened.
    fn opened(&mut self) -> bool {
        let Some(open) = self.opening.take() else {
            return false;
        };
        match open {
            runtime_portal::process_link::OpenRequest::File(file) => self.opened_file(file),
            runtime_portal::process_link::OpenRequest::Uri(link) => {
                if let Err(why) = self.run_open_link(&link) {
                    self.instance.failed(why);
                }
            }
        }
        true
    }

    /// Run the action `file`'s kind declares for it, and open what it made.
    fn opened_file(&mut self, file: runtime_portal::process_link::OpenedFile) {
        let opening = match &file.name {
            Some(name) => format!("`{name}`"),
            None => "the file".to_owned(),
        };
        // The action its package declares for this kind of file: the extension the name ends in,
        // which is the narrower claim, else the media type the name declares.
        let media =
            tilolang_tilo::runtime::blob::media_of(file.name.as_deref().unwrap_or_default());
        let declared = tilolang_tilo::meta::action_opening(
            &self.bundle.meta,
            file.name.as_deref(),
            &media,
        )
        .cloned();
        let Some(action) = declared else {
            // The app does not open this kind of file: its window shows its records, and says why
            // rather than opening a window that took nothing.
            self.instance
                .failed(format!("`{}` does not open {opening}", self.spec.app));
            return;
        };
        self.open_declared(&action, file, &opening);
    }

    /// Run the package's `open_link` with `link`, and open what it made: a `tube:` link shows
    /// the video, channel or playlist it names, as a press of a row shows one. An app that
    /// declares none says so rather than opening a window that took nothing.
    fn run_open_link(&mut self, link: &str) -> Result<(), String> {
        let Some((target, action)) = tilolang_tilo::meta::open_link(&self.bundle.meta) else {
            return Err(format!("`{}` does not open links", self.spec.app));
        };
        let action = action.clone();
        let target = target.clone();
        let host = self
            .instance
            .host()
            .ok_or_else(|| "the app has not started".to_owned())?;
        let root = host
            .root_resource(&self.spec.app, &target.id)
            .map_err(|error| error.to_string())?;
        let actor = tilolang_tilo::runtime::Actor::Member(MemberId::person(&self.spec.person));
        match host.runtime().invoke_action(
            &actor,
            (
                action.id.as_str(),
                vec![Value::resource(&root), Value::text(link)],
            ),
        ) {
            Ok(InvokeOutcome::Done(Value::Ref(made))) => {
                let made = Rc::unwrap_or_clone(made.target);
                self.instance
                    .opens_what_it_made(tilolang_tilo::publish::resource_id(
                        &self.spec.app,
                        &made.id_string(),
                    ));
                Ok(())
            }
            Ok(InvokeOutcome::Done(_)) => Ok(()),
            Ok(other) => Err(format!("`{link}` was not opened: {other:?}")),
            Err(error) => Err(error.to_string()),
        }
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
        let Some(target) = tilolang_tilo::meta::opening_root(&self.bundle.meta, action) else {
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
            ui_core::content::manifest::whole(read, max, |chunk| {
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
            // Nothing is made: an action that made no resource, and a call that waits on
            // another app or on a person, which answers nobody here.
            Ok(InvokeOutcome::Done(_) | InvokeOutcome::Asked(_)) => None,
            Ok(InvokeOutcome::Waits { run, call }) => {
                self.waiting(call, Some(run), None, None);
                None
            }
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

    /// The records other devices wrote, every one the frame's events named, read again in one
    /// go: what this window read of them is read again, and the other apps whose resources it
    /// reads are read again too, so what it shows moves with them. Whether any moved.
    fn records_moved(&mut self) -> bool {
        if self.moved_records.is_empty() {
            return false;
        }
        let records = std::mem::take(&mut self.moved_records);
        self.instance.records_moved(&records);
        for used in &mut self.used {
            used.moved = true;
        }
        true
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
            // Apps installed or removed since the last frame: what views show of who made a change is published again.
            if tilo_resource_store::history::authors::set_installed_apps(apps.clone()) {
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
                // A call raised for the page's prompt that ended without running is the page's
                // no; one that goes on is followed until it moves.
                match self.page_prompts.answered(&message) {
                    Some(crate::page_prompts::Answered::No(no)) => {
                        if let Err(why) = self.instance.ask_page(&no) {
                            tracing::warn!(%why, "the page was not told no");
                        }
                        return true;
                    }
                    Some(crate::page_prompts::Answered::Goes(invocation)) => {
                        self.observe(invocation, "a prompt's call is not followed");
                        return true;
                    }
                    Some(crate::page_prompts::Answered::Said) => return true,
                    None => {}
                }
                return self.answered(&message);
            }
            CollabEvent::InvocationCancellation { id } => {
                // A run that waits on a call stops that call, by the call's own cancel through
                // the dispatch: its answer ends the run, and the run answers the invocation.
                // A check the run started stops, and the invocation is answered as stopped once
                // it has. Any other invocation ran to its end in the frame it arrived in and was
                // answered there: a cancellation that was not in that frame stops nothing.
                if !self.checks.cancel_delivery(id) && !self.stop_calls_of(id) {
                    tracing::debug!(id, "a cancellation of an invocation already answered");
                }
                return false;
            }
            CollabEvent::WorkspaceControl { .. } => {
                self.not_ours();
                return false;
            }
            CollabEvent::ResourcesArrived { records } => {
                // Records of the workspace the instance reads arrived here from elsewhere: what
                // it read of them is read again, and what it shows moves with it. A move that
                // names no record may have moved any of them.
                if records.is_empty() {
                    self.store_moved();
                    return true;
                }
                self.moved_records.extend(records);
                return false;
            }
            CollabEvent::RecordsMoved { records } => {
                // Another device wrote these records: what this window read of them is read
                // again with the rest of the frame's events, once, however many ops moved them.
                self.moved_records.extend(records);
                return false;
            }
            _ => return false,
        };
        self.serve(Delivered {
            id,
            invocation,
            resource,
            action,
            argument,
            actor,
            context,
        })
    }

    /// Run the invocation the broker delivered, as the invoker it admitted: its action's code, or
    /// what it calls that answers later. Whether it changed the instance.
    fn serve(&mut self, delivered: Delivered) -> bool {
        let Delivered {
            id,
            invocation,
            resource,
            action,
            argument,
            actor,
            context,
        } = delivered;
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
            || person.and_then(collab_types::person_workspace_of) == Some(route.cluster_id);
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
        let serving = Serving {
            invocation,
            person: person.map(str::to_owned),
            action: address.action.clone(),
        };
        SERVING.with(|slot| *slot.borrow_mut() = Some(serving));
        // The person's approve it came back with, what they typed included: a sign-in its action
        // answers is filled from it on the way to the window, and nowhere else.
        let approval = attributed
            .as_ref()
            .and_then(|context| context.approval.clone());
        let outcome = tilolang_tilo::page::answering(approval, || {
            self.invoke(&address, argument.as_deref(), &invoker, attributed.as_ref())
        });
        SERVING.with(|slot| *slot.borrow_mut() = None);
        self.store.serve(None);
        let answer = match outcome {
            // A check the project's host approved that the run started is the invocation's
            // work: it stays open until the check ends, and is answered then.
            Ok(Ran::Done(output)) if self.checks.continues(id, invocation, &output) => None,
            Ok(Ran::Done(output)) => Some(InvocationUpdate::Succeeded { invocation, output }),
            // What it calls answers later: the invocation ends with the run, not now.
            Ok(Ran::Waits { call, run }) => {
                self.waiting(call, run, Some((id, invocation)), person.map(str::to_owned));
                None
            }
            Err(error) => {
                self.checks.abandon(invocation);
                Some(failed(invocation, error))
            }
        };
        // The action's writes reached the store as it ran: what it queued outside the workspace
        // waits in the outbox, and the instance settles it on its next frame, as it does what a
        // person's own action queues.
        self.instance.commit_planned();
        if let Some(answer) = answer {
            self.answer_invocation(id, answer);
        }
        true
    }

    /// Hand the instance the readings of the platform roots its bundle reads, and follow the ones
    /// its views asked for. Whether a reading moved, which runs those views again.
    fn read_platform(&self) -> bool {
        let Some(host) = self.instance.host() else {
            return false;
        };
        let read_now = self.instance.foreign_reads();
        self.platform
            .borrow_mut()
            .read(&host, |target| read_now.contains(target))
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

    /// Keep the child call `call` as one a run waits on, and as a step of the invocation the
    /// broker served (`served`) when it was made under one. The run goes on with the answer, and
    /// the served invocation ends with the run.
    fn waiting(
        &self,
        call: String,
        run: Option<tilolang_tilo::runtime::ids::RunId>,
        served: Option<(u64, tilo_actions::InvocationId)>,
        person: Option<String>,
    ) {
        // The invocation this run serves outlives this host: it is recorded with the run, so a
        // host starting over the store answers it if the run cannot go on.
        if let (Some(run), Some((id, invocation)), Some(host)) =
            (&run, &served, self.instance.host())
            && let Err(error) = host.runtime().serving(run, *id, &invocation.0.to_string())
        {
            tracing::warn!(%run, %error, "the served invocation was not kept with the run");
        }
        self.children.borrow_mut().insert(
            call,
            Child {
                run,
                served,
                person,
                asked: None,
            },
        );
    }

    /// Ask the dispatch how the call `invocation`, which a run waits on, stands once it has moved
    /// on: the answer waits until it ends, or for a while, and then asked again. A call the
    /// dispatch runs in the background is answered this way, never on its own.
    fn follow(&self, invocation: tilo_actions::InvocationId) {
        if !self
            .children
            .borrow()
            .contains_key(&invocation.0.to_string())
        {
            return;
        }
        self.observe(invocation, "a call a run waits on is not followed");
    }

    /// Ask the dispatch how `invocation` stands once it has moved on, or after a while; `unsent`
    /// says what is lost when the ask cannot go out.
    fn observe(&self, invocation: tilo_actions::InvocationId, unsent: &str) {
        let message = tilo_actions::remote::RemoteActionMessage::Observe {
            correlation: 0,
            invocation,
            wait_ms: Some(FOLLOW_MS),
            seen: None,
        };
        if let Err(error) = self
            .connection
            .manager()
            .try_send(CollabRequest::RemoteAction { message })
        {
            tracing::warn!(app = self.spec.app, %error, "{unsent}");
        }
    }

    /// Stop each call a run serving the invocation the dispatch delivered as `served` waits on.
    /// Whether any waits.
    fn stop_calls_of(&self, served: u64) -> bool {
        let calls: Vec<String> = self
            .children
            .borrow()
            .iter()
            .filter(|(_, child)| child.served.is_some_and(|(id, _)| id == served))
            .map(|(call, _)| call.clone())
            .collect();
        for call in &calls {
            let Ok(invocation) = call.parse::<tilo_actions::InvocationId>() else {
                continue;
            };
            let message = tilo_actions::remote::RemoteActionMessage::Cancel { invocation };
            if let Err(error) = self
                .connection
                .manager()
                .try_send(CollabRequest::RemoteAction { message })
            {
                tracing::warn!(app = self.spec.app, %error, "a stopped run's call was not stopped");
            }
        }
        !calls.is_empty()
    }

    /// Say that a task or workspace request reached an app's instance, which does not run them.
    fn not_ours(&self) {
        tracing::warn!(
            app = self.spec.app,
            instance = self.spec.instance,
            "a task or workspace request reached an app instance, which does not run them"
        );
    }

    /// What another app's api answered of a child call: the run waiting on it goes on, and the
    /// invocation the broker served is answered when the run ends. Whether the message was an
    /// answer this instance waits on.
    /// The call `call` a run waits on waits in the asked record `record` for a person's yes: the
    /// invocation the run serves says it is asked, naming that record, as the dispatch says of
    /// any call it keeps asked, so whoever made it reads that it waits for a person and not
    /// that it hangs. Its result follows once they answer. Said once per record.
    fn asked_in(&self, call: tilo_actions::InvocationId, record: String) {
        let served = {
            let mut children = self.children.borrow_mut();
            let Some(child) = children.get_mut(&call.0.to_string()) else {
                return;
            };
            if child.asked.as_deref() == Some(record.as_str()) {
                return;
            }
            child.asked = Some(record.clone());
            child.served
        };
        let Some((id, invocation)) = served else {
            return;
        };
        self.answer_invocation(
            id,
            InvocationUpdate::Progress {
                invocation,
                sequence: 1,
                phase: tilo_actions::remote::asked::ASKED_PHASE.to_owned(),
                completed: None,
                total: None,
                message: Some(record),
            },
        );
    }

    fn answered(&mut self, message: &tilo_actions::remote::RemoteActionMessage) -> bool {
        use tilo_actions::remote::{ActionCallError, ReceiptStatus, RemoteActionMessage};
        if let RemoteActionMessage::Update {
            invocation,
            state: Some(state),
            ..
        } = message
            && let InvocationUpdate::Progress {
                phase,
                message: record,
                ..
            } = &state.update
            && phase == tilo_actions::remote::asked::ASKED_PHASE
        {
            self.asked_in(*invocation, record.clone().unwrap_or_default());
        }
        let receipt = match message {
            RemoteActionMessage::Receipt(receipt) => receipt.clone(),
            RemoteActionMessage::Update {
                state: Some(state), ..
            } => state.receipt(),
            // The dispatch no longer knows a call a run waits on: the run cannot hear from it.
            RemoteActionMessage::Update {
                invocation,
                state: None,
                ..
            } => tilo_actions::remote::InvocationReceipt {
                invocation: *invocation,
                status: ReceiptStatus::Failed {
                    error: tilo_actions::ActionFailure {
                        code: "unknown_invocation".into(),
                        message: "the call is no longer known where it ran".into(),
                        details: None,
                        retryable: false,
                    },
                },
            },
            _ => return false,
        };
        // The dispatch admitted it and runs it: it is followed until it ends.
        if matches!(receipt.status, ReceiptStatus::Accepted) {
            self.follow(receipt.invocation);
            return false;
        }
        // A person is being asked about it: the dispatch keeps it open and answers once they
        // have, so the run keeps waiting.
        if matches!(
            &receipt.status,
            ReceiptStatus::Rejected {
                error: ActionCallError::ConfirmationRequired { .. }
            }
        ) {
            return false;
        }
        let call = receipt.invocation.0.to_string();
        let invocation = self.waiting_calls.borrow_mut().remove(&call);
        let Some(child) = self.children.borrow_mut().remove(&call) else {
            // A call made outside a run (a press, whose answer the dispatch read when it ran
            // the action), or one this instance has already answered.
            return false;
        };
        let answer = match &receipt.status {
            ReceiptStatus::Completed { output } => self.answered_value(invocation.as_ref(), output),
            ReceiptStatus::Failed { error } => Invoked::Failed(error.message.clone()),
            // A call a person did not allow, or one stopped on the way: the run sees a refusal.
            ReceiptStatus::Rejected { error } => Invoked::Refused(error.said()),
            ReceiptStatus::Cancelled { message } => Invoked::Refused(
                message
                    .clone()
                    .unwrap_or_else(|| "the call was stopped".to_owned()),
            ),
            ReceiptStatus::Accepted => return false,
        };
        match (child.run, child.served) {
            (Some(run), served) => self.resume(&run, served, child.person, answer),
            // The call itself is the served invocation's work: its answer ends it.
            (None, Some((id, invocation))) => {
                let update = match receipt.status {
                    ReceiptStatus::Completed { output } => {
                        InvocationUpdate::Succeeded { invocation, output }
                    }
                    ReceiptStatus::Failed { error } => {
                        InvocationUpdate::Failed { invocation, error }
                    }
                    ReceiptStatus::Cancelled { message } => InvocationUpdate::Cancelled {
                        invocation,
                        message,
                    },
                    ReceiptStatus::Rejected { error } => {
                        failed(invocation, Refused::new(error.code(), error.said()))
                    }
                    ReceiptStatus::Accepted => return false,
                };
                self.instance.commit_planned();
                self.answer_invocation(id, update);
            }
            (None, None) => {}
        }
        true
    }

    /// Run the run `run` on with the child call's answer: it either ends, and the invocation
    /// the broker served is answered with what it did, or it waits on another call, kept here
    /// under the id that call went out with.
    fn resume(
        &mut self,
        run: &tilolang_tilo::runtime::ids::RunId,
        served: Option<(u64, tilo_actions::InvocationId)>,
        person: Option<String>,
        answer: Invoked,
    ) {
        let Some(host) = self.instance.host() else {
            return;
        };
        // The run goes on as the invocation the broker served: what it writes from here is that
        // invocation's step, and a call it makes continues it.
        if let Some((_, invocation)) = served {
            self.store.serve(Some(invocation.0.to_string()));
            SERVING.with(|slot| {
                *slot.borrow_mut() = Some(Serving {
                    invocation,
                    person: person.clone(),
                    action: String::new(),
                });
            });
        }
        let outcome = host.runtime().answer_call(run, answer);
        SERVING.with(|slot| *slot.borrow_mut() = None);
        self.store.serve(None);
        match outcome {
            Ok(RunOutcome::Done(value)) => {
                self.instance.commit_planned();
                if let Some((id, invocation)) = served {
                    let output = output_of(&self.spec.app, &value);
                    // A check the run started before it waited is still the invocation's work.
                    if !self.checks.continues(id, invocation, &output) {
                        let update = InvocationUpdate::Succeeded { invocation, output };
                        self.answer_invocation(id, update);
                    }
                }
            }
            Ok(RunOutcome::Waits { run, call }) => self.waiting(call, Some(run), served, person),
            Err(error) => {
                self.instance.commit_planned();
                if let Some((id, invocation)) = served {
                    self.checks.abandon(invocation);
                    let update =
                        failed(invocation, Refused::new("action_failed", error.to_string()));
                    self.answer_invocation(id, update);
                }
            }
        }
    }

    /// What the child call, whose invocation is `invocation`, answers its run as: its output as
    /// the declaring action returns it. A call resumed from a journal (its invocation is not
    /// kept with it) answers nothing, so the run goes on with no result.
    fn answered_value(
        &self,
        invocation: Option<&Invocation>,
        output: &tilo_actions::ActionOutput,
    ) -> Invoked {
        let Some(invocation) = invocation else {
            return Invoked::Done(Value::Unit);
        };
        let output = serde_json::to_value(output).unwrap_or_default();
        self.callees
            .answered(invocation, tilolang_tilo::task::Answered::Done(output))
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
            .read_resource(
                &tilolang_tilo::ReadAsk::of(Some(&edit.resource)),
                &host.reads_for(host.viewer()),
            )
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
    #[allow(
        clippy::too_many_lines,
        reason = "one invocation's routing: the app's edits, its verbs, its host vocabulary and the system's link"
    )]
    fn invoke(
        &mut self,
        address: &ActionAddress,
        argument: Option<&str>,
        invoker: &ActionInvoker,
        ambient: Option<&ActionContext>,
    ) -> Result<Ran, Refused> {
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
            return Ok(Ran::Done(tilo_actions::ActionOutput {
                value,
                resources: Vec::new(),
            }));
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
                &self.spec.person,
            )
            .map(Ran::Done);
        }
        if encoded.type_id == tilo_resource::read::TYPE {
            return self
                .read(&encoded.payload, &caller.reader())
                .map_err(|error| Refused::new("action_failed", error))
                .map(Ran::Done);
        }
        if encoded.type_id == tilo_actions::remote::APPLICATION_ACTIVATE_TYPE {
            self.instance.activate_window();
            return Ok(Ran::Done(tilo_actions::remote::activation_requested()));
        }
        if encoded.type_id == ACTION_TYPE {
            let action: DynAction = serde_json::from_slice(&encoded.payload)
                .map_err(|error| Refused::new("action_invalid_arguments", error.to_string()))?;
            if let Some(why) = self.store.take_refusal() {
                return Err(Refused::new("action_failed", why));
            }
            // What the action asked of another app, or of the app's own action the authority
            // asks a person about, went through the dispatch as a child call: the invocation
            // waits on its answer, and is answered when it comes.
            if let Some(waiting) = host.take_waiting() {
                return Ok(Ran::Waits {
                    call: waiting.call,
                    run: waiting.run,
                });
            }
            // The answer names the characters it wrote: the window draws them in the writer's
            // colour, ending at the writer's caret.
            let mut output = returned(&host, &action)?;
            if let Some(wrote) = self.store.take_wrote(&action.app) {
                output.value = tilo_actions::ActionOutput::noting_wrote(output.value, &wrote);
            }
            return Ok(Ran::Done(output));
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
            return Ok(Ran::Done(output));
        }
        if encoded.type_id == <OpenEffect as tilo_resource::Effect>::OPERATION {
            return self
                .open_from_action(&encoded.payload, by_person)
                .map(Ran::Done);
        }
        if tilo_resource::share::handles(&encoded.type_id) {
            return self.shared(encoded.payload.as_slice()).map(Ran::Done);
        }
        if encoded.type_id == tilo_actions::remote::APPLICATION_OPEN_LINK_TYPE {
            return self.open_link_from_action(&encoded.payload).map(Ran::Done);
        }
        Err(Refused::new(
            "action_unknown",
            format!("the tilolang host does not run `{}`", encoded.type_id),
        ))
    }

    /// The link to the resource the `share-link` frame `payload` names, minted under the
    /// workspace's page as the session named it to this host: the frame is the app's own, so a
    /// person copying a link, the palette and an agent asking for one all hear the same URL.
    fn shared(&self, payload: &[u8]) -> Result<tilo_actions::ActionOutput, Refused> {
        tilolang_tilo::share::answer(payload, &self.bundle.meta.app, self.spec.link_home.as_ref())
            .map_err(|why| Refused::new("action_failed", why))
    }

    /// Show what the link the system's `open-link` carries names, by running the app's own
    /// `open_link` with it: the same path a window opened with the link takes, so a link a shell
    /// delivers reaches the app whether or not a window of it was open.
    fn open_link_from_action(
        &mut self,
        payload: &[u8],
    ) -> Result<tilo_actions::ActionOutput, Refused> {
        let arguments: serde_json::Value = tilo_actions::decode_json(payload)
            .map_err(|error| Refused::new("action_invalid_arguments", error.to_string()))?;
        let link = arguments
            .get(tilo_actions::remote::OPEN_LINK_PARAM)
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                Refused::new(
                    "action_invalid_arguments",
                    "open-link needs the link to open".to_owned(),
                )
            })?;
        self.run_open_link(link)
            .map_err(|why| Refused::new("action_failed", why))?;
        Ok(tilo_actions::ActionOutput::default())
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
    /// sentence it reads as in `locale`, as the person `person` reads them.
    fn history_view(
        surface: &Surface,
        shown: &[String],
        address: &ActionAddress,
        payload: &[u8],
        host: &Host,
        locale: &str,
        person: &str,
    ) -> Result<tilo_actions::ActionOutput, Refused> {
        use tilo_resource_store::history::verbs::HistoryView;
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
        let reader = tilolang_tilo::runtime::Actor::Member(MemberId::person(person));
        let mut value = tilolang_service::history(host, &trees, &view, &reader)
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
        let value = host.answer_read(&args, reader)?;
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
            // The system's own verbs every resource with a view offers, which this host mints
            // ([`tilolang_tilo::share`]) rather than the app.
            tilo_resource::share::SHARE_TYPE.to_owned(),
            tilo_resource::share::SHARE_INVITE_TYPE.to_owned(),
            // The link into an app that opens links, which this host answers by running the
            // app's own `open_link` ([`Self::open_link_from_action`]).
            tilo_actions::remote::APPLICATION_OPEN_LINK_TYPE.to_owned(),
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
            if self.published.is_none() {
                // The window may hold a surface the broker did not take: it holds none now.
                say_surface(
                    &mut self.instance,
                    &SurfaceDelta {
                        reset: true,
                        ..SurfaceDelta::default()
                    },
                );
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

    /// Run the next action the person chose at the window as their own step: the same api an
    /// invocation the broker delivered for them takes, so the action runs for them however it
    /// reached the app. One a frame, so what a press opened is sent, and the window hears it
    /// ran, before the press after it ([`tilolang_tilo::instance::Instance::show_ran`]).
    /// Whether there was one.
    fn chosen(&mut self) -> bool {
        let Some((press, address, argument)) = self.instance.take_chosen() else {
            return false;
        };
        // A check the project's host approved runs for an invocation the workspace holds, which
        // its invoker can follow and stop: the press goes through the dispatch, and the check
        // runs when the broker delivers it back.
        if address.action == collaboration::APPROVED_CHECK_ACTION {
            self.dispatch_press(&address, argument.as_deref());
            self.instance.press_ran(press);
            return true;
        }
        let invoker = ActionInvoker::person(self.spec.person.clone());
        let opens_what_it_made = self.is_primary(&address);
        match self.invoke(&address, argument.as_deref(), &invoker, None) {
            Ok(Ran::Done(output)) => {
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
            // It calls another app and waits on the answer: the run goes on when it comes. A
            // press answers nobody, so nothing is answered for it.
            Ok(Ran::Waits { call, run }) => self.waiting(call, run, None, None),
            Err(refused) => self.instance.failed(refused.message),
        }
        // What the press queued outside the workspace waits in the outbox, and the instance
        // settles it on its next frame, as it does a delivered invocation's: a press that only
        // reaches outside (Check now) writes nothing else that would wake it.
        self.instance.commit_planned();
        // The press ran: once it is in the document this frame sends what it opened, then tells
        // the window it ran, so what the person typed after it goes there.
        self.instance.press_ran(press);
        true
    }

    /// Send the person's press of `address` with `argument` through the dispatch, as the app's
    /// call of its own action: the broker admits it as theirs and delivers it back to run.
    fn dispatch_press(&mut self, address: &ActionAddress, argument: Option<&str>) {
        use tilo_actions::remote::{InvokeArgument, InvokeRequest, RemoteActionMessage};
        let given = argument.map_or("{}", |given| {
            given
                .strip_prefix(tilo_actions::ARGUMENTS_JSON_PREFIX)
                .unwrap_or(given)
        });
        let arguments = match serde_json::from_str::<serde_json::Value>(given) {
            Ok(serde_json::Value::Object(arguments)) => arguments,
            Ok(other) => {
                self.instance.failed(format!(
                    "The check's arguments are not named values: {other}"
                ));
                return;
            }
            Err(error) => {
                self.instance
                    .failed(format!("The check's arguments could not be read: {error}"));
                return;
            }
        };
        let request = InvokeRequest {
            invocation: tilo_actions::InvocationId::new(),
            target: tilo_actions::remote::application_action_target(
                &self.spec.workspace,
                &self.spec.app,
                address.resource.clone(),
                address.action.clone(),
            ),
            argument: InvokeArgument::Json(arguments),
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        };
        let message = RemoteActionMessage::Invoke(request);
        if !self
            .connection
            .manager()
            .send(CollabRequest::RemoteAction { message })
        {
            self.instance
                .failed("The workspace could not be reached to run the check".to_owned());
        }
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

    /// Hand the window and the broker `delta` of the instance's surface. Whether the broker took
    /// it: a document on this device alone has no broker, and its window's catalogue is all there
    /// is to publish to. The window takes it whether or not the broker does: a surface the broker
    /// did not take is sent again whole, which the window takes as well.
    fn send_surface(&mut self, delta: SurfaceDelta) -> bool {
        say_surface(&mut self.instance, &delta);
        self.connection.is_local()
            || self
                .connection
                .manager()
                .send(CollabRequest::PublishActionSurface { delta })
    }

    /// What the instance holds is in the store as it is written: nothing waits as it ends.
    pub fn flush(&mut self) {}

    /// The window closed, and its page with it: every call raised for what the page waited on is
    /// withdrawn, so its person is asked no more.
    pub fn page_closed(&mut self) {
        let connection = &self.connection;
        self.page_prompts
            .withdraw_all(|request| connection.manager().send(request));
    }

    /// What the workspace's document of each person's own state holds, as this instance holds it.
    #[must_use]
    pub fn personal(&self) -> &tilolang_tilo::personal::PersonalState {
        self.personal.state()
    }
}

/// How long one ask of how a call stands waits while the call has not moved: as long as the
/// dispatch holds one ([`tilo_actions::remote::MAX_REMOTE_WAIT`]).
const FOLLOW_MS: u32 = 10_000;

/// Where the runtime sends the calls an app makes of another app's action, or of its own action
/// the authority asks a person about first: each goes through the dispatch at once, as a child of
/// the invocation being served, and the run that made it waits on the answer
/// ([`Hosted::answered`]).
struct Apis {
    /// The workspace the call is made in.
    workspace: String,
    /// How a call of another app's action is built, from the packages this app may use.
    callees: Rc<Callees>,
    /// The invocation each child call went out as, by the id it went out under.
    calls: Rc<RefCell<BTreeMap<String, Invocation>>>,
    /// The instance's connection, to send the call on.
    manager: Rc<RefCell<collaboration::CollaborationManager>>,
    /// The platform packages this window reads: their resources are no records here, and what a
    /// view changes of one runs where the session serves it, before the calling action goes on.
    platform: Rc<RefCell<Roots>>,
}

impl Invoker for Apis {
    fn invoke(&self, invocation: &Invocation) -> Invoked {
        // A platform app's resource is answered by the session's services, and its refusal is
        // the calling action's result, not a change that lands later as a reading.
        if let Some((app, resource, action, tilo_actions::remote::InvokeArgument::Json(arguments))) =
            api_ask(invocation)
            && self.platform.borrow().reads(&app)
        {
            use tilolang_tilo::runtime::Value;
            use tilolang_tilo::runtime::delegate::Invoked;
            return match self
                .platform
                .borrow()
                .invoke(&resource, &action, &arguments)
            {
                Ok(()) => Invoked::Done(Value::Unit),
                Err(refusal) => Invoked::Refused(refusal),
            };
        }
        let call = match self.callees.call(invocation) {
            Ok(call) => call,
            Err(answer) => return self.callees.answered(invocation, answer),
        };
        let field = |key: &str| call.get(key).cloned().unwrap_or_default();
        let text = |key: &str| field(key).as_str().unwrap_or_default().to_owned();
        let app = text("app");
        let invocation_id = tilo_actions::InvocationId::new();
        // The call continues the invocation the broker is serving on this thread, decided as
        // its caller's. A call the authority asks a person about goes the same way: the
        // dispatch keeps it as its invocation record, asked, and answers once they have
        // (`delegate::Invocation::Action::asks`).
        let served = SERVING.with(|served| served.borrow().clone());
        // A call of a host's own record (the shell's asking view resolving an invocation) names
        // its provider itself; every other goes to the action's app in this workspace.
        let workspace = match text("workspace") {
            named if !named.is_empty() => named,
            _ => self.workspace.clone(),
        };
        let target = match call.get("provider").and_then(serde_json::Value::as_str) {
            Some(provider) => tilo_actions::remote::RemoteActionTarget {
                provider: provider.to_owned(),
                address: tilo_actions::ActionAddress::new(text("resource"), text("action")),
            },
            None => tilo_actions::remote::application_action_target(
                &workspace,
                &app,
                text("resource"),
                text("action"),
            ),
        };
        let request = tilo_actions::remote::InvokeRequest {
            invocation: invocation_id,
            target,
            argument: tilo_actions::remote::InvokeArgument::Json(
                field("arguments").as_object().cloned().unwrap_or_default(),
            ),
            context: asked_facts(invocation),
            confirmation: None,
            revision: None,
            on_behalf_of: served.as_ref().map(|served| served.invocation),
            wait_ms: None,
            appearance: None,
            signed: None,
        };
        tracing::debug!(
            to = app,
            on_behalf_of = served
                .as_ref()
                .map(|served| served.invocation.0.to_string()),
            person = served.as_ref().and_then(|served| served.person.as_deref()),
            "a child call goes through the dispatch"
        );
        let message = tilo_actions::remote::RemoteActionMessage::Invoke(request);
        if let Err(error) = self
            .manager
            .borrow_mut()
            .try_send(CollabRequest::RemoteAction { message })
        {
            tracing::warn!(to = app, %error, "a child call did not reach the dispatch");
            return Invoked::Failed(format!("the call did not reach {app}: {error}"));
        }
        let call_id = invocation_id.0.to_string();
        self.calls
            .borrow_mut()
            .insert(call_id.clone(), invocation.clone());
        Invoked::Pending(call_id)
    }
}

/// What the person asked to allow `invocation` reads on the card: the facts the runtime read for
/// it through the run that makes it (a draft the run just wrote), which only this app can read
/// yet. The dispatch takes them only from the callee's own app.
fn asked_facts(invocation: &Invocation) -> Option<ActionContext> {
    let Invocation::Action { shows, .. } = invocation else {
        return None;
    };
    if shows.is_empty() {
        return None;
    }
    Some(ActionContext {
        shows: shows
            .iter()
            .map(|shown| tilo_actions::remote::asked::Fact::new(&shown.label, &shown.value))
            .collect(),
        ..ActionContext::default()
    })
}

/// Every package this app may call, by app, in the workspace it calls them in: what a child call
/// is built from. A package a registered provider publishes is called at the addresses it
/// registered (`provided`).
fn callees_of(
    bundle: &Bundle,
    workspace: &str,
    provided: &[tilo_actions::remote::ProvidedPackage],
) -> Callees {
    let mut callees = Callees {
        apps: bundle
            .imports
            .iter()
            .map(|import| &import.meta)
            .chain([&bundle.meta])
            .map(|meta| (meta.app.clone(), meta.clone()))
            .collect(),
        workspace: Some(workspace.to_owned()),
        ..Default::default()
    };
    // The platform's packages the app uses, which this process has no module for: what the
    // registry's own verbs (`tilo.actions`) are declared by, and what their answers read as.
    for app in &bundle.meta.uses {
        if let Ok(meta) = tilolang_tilo::platform_apps::platform_package(app) {
            callees.apps.insert(app.clone(), meta);
        }
    }
    callees.provide(provided);
    callees
}

/// What the action whose run `value` returned answers: the resource it made, if any, as the
/// invocation answers it.
fn output_of(app: &str, value: &Value) -> tilo_actions::ActionOutput {
    match value {
        Value::Ref(made) => tilo_actions::ActionOutput {
            value: None,
            resources: vec![tilolang_tilo::publish::resource_id(
                app,
                &made.target.id_string(),
            )],
        },
        _ => tilo_actions::ActionOutput::default(),
    }
}

/// What a view or an action of this window asks of another app's api: the app, the resource,
/// the action's name and its arguments as the api takes them. `None` for an invocation that
/// names no target.
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

impl Drop for InRun {
    fn drop(&mut self) {
        self.0.set_run(None);
    }
}

/// What the action `action` returned: the value its declaration carries as data, and the
/// resource it made, by its published id, which a task's program goes on with.
fn returned(
    host: &tilolang_tilo::host::Host,
    action: &DynAction,
) -> Result<tilo_actions::ActionOutput, Refused> {
    let Some(value) = host.take_returned(&action.action_id) else {
        return Ok(tilo_actions::ActionOutput::default());
    };
    host.action_output(&action.app, &action.action_id, &value)
        .map_err(|error| Refused::new("action_failed", error))
}

/// Tell the window of `instance` the change `delta` of the surface the instance publishes, as
/// the broker takes it: the actions the person's command palette offers there.
fn say_surface(instance: &mut Instance, delta: &SurfaceDelta) {
    let bytes = match serde_json::to_vec(delta) {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::error!(%error, "an action surface did not encode");
            return;
        }
    };
    instance.say(ToUi::Actions(bytes));
    // The window takes it now, not on the frame after.
    instance.flush();
}

/// A content store held in memory: what an instance on this device alone keeps its content in.
#[must_use]
pub fn memory_content() -> tilo_content::ContentStore {
    tilo_content::ContentStore::memory()
}

/// The Playback action the media control `command` runs, and the arguments it takes beside the
/// Playback: the resource the person's controls act on.
fn media_action(
    command: runtime_portal::process_link::MediaCommand,
    playing: &tilolang_tilo::runtime::ResourceRef,
    length: f64,
) -> (&'static str, Vec<Value>) {
    let target = Value::resource(playing);
    match command {
        runtime_portal::process_link::MediaCommand::Play => ("play", vec![target]),
        // The player has no stop of its own: a stop stands it still, which is its pause.
        runtime_portal::process_link::MediaCommand::Pause
        | runtime_portal::process_link::MediaCommand::Stop => ("pause", vec![target]),
        runtime_portal::process_link::MediaCommand::Toggle => ("toggle", vec![target]),
        runtime_portal::process_link::MediaCommand::Next => ("next", vec![target]),
        runtime_portal::process_link::MediaCommand::Previous => ("previous", vec![target]),
        runtime_portal::process_link::MediaCommand::SeekBy(micros) => {
            ("seek_by", vec![target, Value::Int(micros / 1_000_000)])
        }
        runtime_portal::process_link::MediaCommand::SeekTo(micros) => {
            #[expect(
                clippy::cast_precision_loss,
                reason = "a playhead in whole microseconds reads as a place in the video"
            )]
            let place = if length > 0.0 {
                micros as f64 / 1_000_000.0 / length
            } else {
                0.0
            };
            ("seek", vec![target, Value::Float(place.clamp(0.0, 1.0))])
        }
        runtime_portal::process_link::MediaCommand::SetVolume(level) => (
            "set_volume",
            vec![target, Value::Float(level.clamp(0.0, 1.0))],
        ),
        runtime_portal::process_link::MediaCommand::Mute => ("mute", vec![target]),
    }
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

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::VecDeque;

    use tilo_resource_store::wire::{Bytes, Command, Reply};
    use tilo_resource_store::{LocalPort, Port};

    use super::read_earlier;

    const APP: &str = "so.tilo.lang.notes";

    /// A document whose `title` text is `title`.
    fn titled(title: &str) -> automerge::AutoCommit {
        use automerge::transaction::Transactable as _;
        let mut doc = automerge::AutoCommit::new();
        let text = doc
            .put_object(automerge::ROOT, "title", automerge::ObjType::Text)
            .unwrap();
        doc.splice_text(&text, 0, 0, title).unwrap();
        doc.commit();
        doc
    }

    /// A port answering a script of replies: what a workspace whose documents are on their way,
    /// and one that answers more than fits, send a read.
    #[derive(Default)]
    struct Script {
        replies: RefCell<VecDeque<Reply>>,
        asked: RefCell<Vec<Command>>,
    }

    impl Script {
        fn answering(replies: Vec<Reply>) -> Self {
            Self {
                replies: RefCell::new(replies.into()),
                asked: RefCell::default(),
            }
        }

        fn asked(&self) -> usize {
            self.asked.borrow().len()
        }
    }

    impl Port for Script {
        fn command(&self, command: Command) -> Result<Reply, String> {
            self.asked.borrow_mut().push(command);
            Ok(self
                .replies
                .borrow_mut()
                .pop_front()
                .expect("a scripted answer"))
        }
    }

    /// A record's answer in one page, `pending` when it is on its way.
    fn page(doc: Option<&mut automerge::AutoCommit>, pending: bool) -> Reply {
        let (changes, heads) = doc.map_or_else(Default::default, |doc| {
            let heads = doc.get_heads().iter().map(ToString::to_string).collect();
            (doc.save_after(&[]), heads)
        });
        Reply::RecordChanges {
            changes: Bytes(changes),
            heads,
            next: None,
            pending,
        }
    }

    #[test]
    fn what_an_earlier_app_kept_reads_whole_by_id() {
        let port = LocalPort::in_memory().named(APP);
        for (id, title) in [("doc-1", "Notes"), ("doc-2", "Tasks")] {
            port.command(Command::RecordChange {
                record: Some(id.to_owned()),
                changes: Bytes(titled(title).save()),
                more: false,
                commit: None,
            })
            .unwrap();
        }
        let read = read_earlier(&port, APP);
        let ids: Vec<&str> = read.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(ids, ["doc-1", "doc-2"], "each record, by its id");
    }

    #[test]
    fn a_record_on_its_way_carries_nothing() {
        let listed = Reply::Ids(vec!["doc-1".to_owned(), "doc-2".to_owned()]);
        let script = Script::answering(vec![
            listed,
            page(Some(&mut titled("Notes")), false),
            page(None, true),
        ]);
        assert!(
            read_earlier(&script, APP).is_empty(),
            "carrying one of them would leave the rest behind"
        );
        assert_eq!(script.asked(), 3, "listed, then each read once");
    }
}
/// The code a vault item's reading states: the digits, how long they last, and how the service
/// makes them.
fn vault_reading(text: &str) -> Option<tilolang_tilo::runtime::host_work::VaultCode> {
    #[derive(serde::Deserialize)]
    struct Reading {
        code: String,
        until: String,
        period: i64,
        digits: i64,
        algorithm: String,
    }
    let reading: Reading = serde_json::from_str(text).ok()?;
    Some(tilolang_tilo::runtime::host_work::VaultCode {
        code: reading.code,
        until: reading.until,
        period: reading.period,
        digits: reading.digits,
        algorithm: reading.algorithm,
    })
}
