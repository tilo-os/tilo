//! Declaring and running a framework app on the toolkit's component runtime.

use std::any::TypeId;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, Mutex, OnceLock};

use tilo_resource::{Invoke, Reader, Resource};
use tilo_ui_base::i18n::EmbeddedCatalog;
use tilo_ui_base::permissions::MemberId;
use tilo_ui_base::widget::Element;
use tilo_ui_base::{
    permissions::ActionKind, permissions::LivePermissions, permissions::PermissionScope,
};
use ui_core::application::{
    AppComponent, ApplicationDefinition, ComponentBehaviour, ComponentFactory, LaunchTarget,
    ProcessContext, RestoreInput,
};
use ui_core::launch::LaunchError;
use ui_core::resource::{DocumentId, ResourceType};
use ui_core::storage::AppStorage;
use ui_core::widget::IconDescriptor;

// Launching (live windows, captures, headless drives, test hosts) discovers
// native presentation and profiles; a browser host prepares components itself.
#[cfg(not(target_arch = "wasm32"))]
use {
    tilo_ui_base::permissions::{PermissionContext, Role},
    ui_core::application::{
        ApplicationOwner, Capture, ComponentRunnerInputs, NativeProcess, PreparedComponentInputs,
    },
    ui_core::headless::Driver,
    ui_core::launch::{LaunchArgs, LaunchSpec},
    ui_core::resource::DocumentCheckout,
};

use super::dispatch::Dispatcher;
use super::effect::Effects;
use super::launch::{Command, Helper, LaunchInput};
use super::live::Live;
use super::root::ResourceRoot;
use super::scenario::{Fixture, Launched, Run};
use super::view::ViewCx;
use tilo_lock::MutexRecover as _;

/// The id, title and icon of an app named at launch ([`ResourceApp::named`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AppName {
    pub id: &'static str,
    pub title: &'static str,
    pub icon: Option<&'static str>,
}

/// A framework app: one resource type and a view of it.
///
/// Everything else — storage, sync, collaboration, history, undo,
/// permissions, the action palette, the agent and script API, search — is
/// provided. See the [module docs](super).
pub trait ResourceApp: 'static {
    /// The resource this app edits.
    type Resource: Resource;
    /// The application id, e.g. `"so.tilo.scratchpad"`.
    const ID: &'static str;
    /// What the app is called when no catalogue names it: its source
    /// language's name, for a host that shows the app before it has one (a
    /// launch listing a package it has not run). An app that
    /// [ships a catalogue](Self::catalog) names its title there instead, in
    /// [`Self::TITLE_KEY`], so a German window is not titled in English.
    const TITLE: &'static str = "";
    /// The catalogue message that names the app, in every language the app
    /// ships, and what its window, its launcher entry and its dock row read.
    /// Every app with a catalogue sets it.
    const TITLE_KEY: &'static str = "";
    /// A named icon; defaults to the app id's last segment.
    const ICON: Option<&'static str> = None;
    /// How every window hosting the app treats it: focus after a rebuild,
    /// live appearance, and whether captures withhold it. An app that shows
    /// secrets declares `capture_protected` here, so the live launch, the
    /// shell and every other host of its component protect it alike; an
    /// on-screen keyboard declares its window an
    /// [input surface](ui_core::application::WindowRole::InputSurface).
    const BEHAVIOUR: ComponentBehaviour = ComponentBehaviour::DEFAULT;
    /// Media types (`type/subtype`, lowercase) of the files the app opens
    /// when a host hands it one (a file association, a file dropped on its
    /// icon, a path of that type on its command line); [`Self::launch`]
    /// receives the file. `"*/*"`
    /// ([`ANY_MEDIA_TYPE`](ui_core::application::ANY_MEDIA_TYPE)) opens every
    /// file, a file whose name says nothing of its type as
    /// `application/octet-stream`.
    const OPENS: &'static [&'static str] = &[];
    /// How many arguments (paths, URLs) a launch may name;
    /// [`Self::launch_resource`] and [`Self::launch`] receive them.
    const ARGUMENTS: usize = 0;
    /// Whether the resource keeps document state whose versions History
    /// shows. An app whose state lives outside its document (a measurement,
    /// a device) sets `false`: it publishes no History actions and its
    /// window offers no History.
    const DOCUMENT_HISTORY: bool = true;
    /// Every window of the app in a workspace opens its one record there, which the first one
    /// creates.
    const WORKSPACE_RESOURCE: bool = false;
    /// Each window shows a resource of its own: a document an editor opened
    /// (Word Processor, Code Editor, Sheets), a terminal session. A window
    /// that names none starts a new one, and two windows show two of them
    /// (or one document together, when both open it); no window mirrors
    /// another. The documents earlier windows started stay in the workspace,
    /// where system search finds them, and one opens in a window with
    /// [`ResourceEnv::open_here`](super::ResourceEnv::open_here).
    /// Without it the app's record is its library and settings,
    /// one per workspace, which every window of the app there shows.
    const WINDOW_RESOURCE: bool = false;
    /// The app's own command-line options that take a value
    /// (`--purpose number` or `--purpose=number`): how this window runs
    /// rather than what it opens. [`Self::launch`] reads them from its
    /// [`LaunchInput::option`], views and surfaces from
    /// [`ResourceEnv::launch_option`](super::ResourceEnv::launch_option).
    const OPTIONS: &'static [&'static str] = &[];
    /// The app's own switches, which take no value (`--debug-hit-areas`):
    /// [`LaunchInput::flag`], [`ResourceEnv::launch_flag`](super::ResourceEnv::launch_flag).
    const FLAGS: &'static [&'static str] = &[];
    /// Whether the app opens private windows (`--private`): a window on a
    /// profile of its own that is thrown away with the process — its record,
    /// settings and files never reach the person's — and that attaches to
    /// no runtime, so nothing it shows syncs or is shared. It holds none of
    /// the person's own records ([`ResourceEnv::profile_resource`](super::ResourceEnv::profile_resource)
    /// answers `None`), and its view tells with [`ResourceEnv::is_private`](super::ResourceEnv::is_private).
    const PRIVATE_WINDOWS: bool = false;
    /// The History of the folder a window shows: the changes of the workspace's tree, the one
    /// the window's rows show, each named by the path of what it changed and by whoever made
    /// it. The runner opens it with the window's storage, so History reads what the rows
    /// read. The view narrows it to the file it shows with
    /// [`ResourceEnv::folder_history`](super::ResourceEnv::folder_history).
    const FOLDER_HISTORY: Option<FolderHistory> = None;
    /// What of the machine it runs on the app shows (its journal, its
    /// processes, its drives, the person's real files). A headless drive
    /// that captures it must then name one of its [scenarios](Self::scenarios)
    /// and is built from that scenario's fixtures, so no picture of the app
    /// shows the machine of whoever took it.
    const READS_HOST: &'static [ui_core::launch::HostSource] = &[];

    /// The app this process is, when only the launch knows it: a runtime that runs a package
    /// (a tilolang app) is the package's app, with its id, title and icon, rather than
    /// [`Self::ID`], [`Self::TITLE`] and [`Self::ICON`]. Asked once per process, before the
    /// definition is first made.
    fn named() -> Option<AppName> {
        None
    }

    /// Build the view. Called once for the live app, and again for every
    /// historical version History mode shows (with a read-only context), so
    /// it must build widgets and nothing else.
    fn view(cx: &mut ViewCx<Self::Resource>) -> Element;

    /// The window shows before the workspace's record is read: an app that keeps
    /// what its first frame shows beside the record (a tilolang app's
    /// index) opens a record this device holds off the frame, shows
    /// [`Self::opening`] meanwhile, and builds [`Self::view`] once the record
    /// is had. Only for an app that keeps one record per workspace.
    const OPENS_BEFORE_RESOURCE: bool = false;

    /// What the window shows while the workspace's record is read off the frame
    /// ([`Self::OPENS_BEFORE_RESOURCE`]); `None` waits for the record. What it
    /// returns is handed to [`Self::view`] once the record is had, as a
    /// service lent to the window ([`ResourceEnv::take_lent`](super::ResourceEnv::take_lent)):
    /// the view keeps showing it rather than building it again.
    fn opening(cx: &OpeningCx<'_>) -> Option<Box<dyn OpeningView>> {
        let _ = cx;
        None
    }

    /// The label of the open resource in the palette and in history.
    fn label(state: &<Self::Resource as Resource>::State) -> String {
        let _ = state;
        <Self::Resource as Resource>::LABEL.to_owned()
    }

    /// View and effect operations of this app's presentation, published on
    /// the same resource as [`Resource::local_operations`]. A shared resource
    /// type (rich text) does not know about one app's chrome. Document
    /// mutations do not belong here. History does not either: it is the
    /// resource's own history, not a toggle beside it.
    fn local_operations() -> Vec<tilo_actions::ActionSpec> {
        Vec::new()
    }

    /// The record a launch naming `input` opens: the file's own record
    /// ([`DocumentId::identify_file`]), a record derived from it
    /// ([`DocumentId::derived`]), a minted one. `None`, the default, opens the
    /// record the app would open anyway: the one the window showed last, else
    /// the workspace's default. Asked by a launcher that picks the record (the
    /// command line, a test host), before the record is opened, so it must
    /// not depend on anything but `input`.
    fn launch_resource(input: &LaunchInput<'_>) -> Option<DocumentId> {
        let _ = input;
        None
    }

    /// The actions a launch naming `input` takes once its record is open;
    /// `created` when this launch created the record, so actions that fill
    /// a new record (the source of a new project) are taken only then. They
    /// go through the same API as every control: checked against the
    /// member's permissions, attributed, undoable and in history. A
    /// launch that shows something rather than changing the resource (a
    /// link to one item, a file to look at before acting on it) invokes one
    /// of the app's effects ([`Invoke::effect`]) through the same API. A
    /// capture launched on nothing takes none: its [scenario](Self::scenario)
    /// stages the record.
    ///
    /// When the record was already there, [`LaunchInput::record`] reads it
    /// as it is: a launch on a file can tell that the file changed since
    /// the record last read or wrote it.
    fn launch(input: &LaunchInput<'_>, created: bool) -> Vec<Invoke<Self::Resource>> {
        let _ = (input, created);
        Vec::new()
    }

    /// What showing `link` takes: the effects (or edits) that bring up what
    /// the link names, `None` for a link this app does not open. A link
    /// reaches it however it arrives: the framework's `open-link` action,
    /// which a shell invokes on the app that claims the link's scheme
    /// (`[providers] schemes` in its manifest) whether or not a window of it
    /// runs, and a launch naming the link, whose
    /// [`launch`](Self::launch) is then not asked.
    fn open_link(link: &str) -> Option<Vec<Invoke<Self::Resource>>> {
        let _ = link;
        None
    }

    /// Whether a launch naming a link shows it through
    /// [`open_link`](Self::open_link). `false` for an app whose window is the
    /// thing it was launched on, which its view reads from the launch itself
    /// (Browser's window is the page it opened on); a link handed to a window
    /// already open still goes through `open_link`.
    const OPENS_LAUNCH_LINK: bool = true;

    /// What a record a launch naming `input` creates starts with besides
    /// the resource's [`initialize`](Resource::initialize): the content of
    /// the file it was launched on. The actions are written into the
    /// record's genesis, so they must depend on nothing but `input` — two
    /// devices that each create the record of one file converge on one
    /// document only if both write the same genesis. They are nobody's
    /// edit: no history entry, no undo.
    fn genesis(input: &LaunchInput<'_>) -> Vec<Invoke<Self::Resource>> {
        let _ = input;
        Vec::new()
    }

    /// The commands the app's binary runs instead of opening a window.
    fn commands() -> &'static [Command] {
        &[]
    }

    /// The helpers the app's binary also is ([`Helper`]), asked in order
    /// before the app starts.
    fn helpers() -> &'static [Helper] {
        &[]
    }

    /// Whether the app takes part in the workspace runtime of the workspace it is
    /// launched in. An app that only runs on this device (an imager on a
    /// platform without the runtime) says no.
    const ATTACHES_RUNTIME: bool = true;

    /// The app's message catalog, loaded before its view is built in every
    /// host that mounts the app, so the view's lookups find its messages.
    fn catalog() -> Option<&'static EmbeddedCatalog> {
        None
    }

    /// Named screenshot scenarios for `--shots --scenario NAME`; `--shots-dir`
    /// captures every one.
    fn scenarios() -> &'static [&'static str] {
        &[]
    }

    /// The viewport a capture of `scenario` (`None` for a plain `--shots`)
    /// is drawn at, unless `--viewport` names one: a narrow scenario's
    /// phone-sized window.
    fn shot_viewport(scenario: Option<&str>) -> (u32, u32) {
        let _ = scenario;
        (1280, 800)
    }

    /// Prepare what a capture of `fixture.scenario()` shows that its actions
    /// cannot stage, before the view is built: typed stand-ins for live
    /// sources, scratch files, content-store blobs, other people's versions
    /// and members. Runs only for a capture, whose views are built in
    /// [`RunMode::Capture`](super::RunMode). See [`Fixture`](super::Fixture).
    fn fixture(fixture: &mut super::Fixture<Self::Resource>) {
        let _ = fixture;
    }

    /// The actions that stage the resource for a capture: `Some(name)` for
    /// `--shots --scenario NAME`, `None` for a plain `--shots`. They apply
    /// through the same API as every control; then the views stage their
    /// view state ([`ViewCx::on_stage`], `ResourceSurface::stage`) and the
    /// frame is captured. See [`stage`](super::stage).
    fn scenario(name: Option<&str>) -> Vec<Invoke<Self::Resource>> {
        let _ = name;
        Vec::new()
    }

    /// What a person does in the scene that the scenario's actions and
    /// staged view state cannot show: text typed into an editor, a drag
    /// held over its drop target, a menu opened with a right click. Driven
    /// on `input` through the window's own event path after the actions
    /// and staging, so the frame shows the app's real answer to that input.
    /// Must be deterministic, like the rest of the scenario.
    fn scenario_input(
        name: Option<&str>,
        input: &mut ui_core::headless::Driver,
    ) -> Result<(), String> {
        let _ = (name, input);
        Ok(())
    }

    /// What a brand-new record starts with that depends on who creates it or
    /// where: the creator as its owner, a first-run import of settings that
    /// already exist on this device. Everything that depends on neither is
    /// the resource's [`initialize`](Resource::initialize), which is the same
    /// on every device so that two people creating the same record converge
    /// on one.
    ///
    /// The actions apply once, right after the genesis, attributed to the
    /// creator and recorded as content from outside: they sync like any
    /// edit but are nobody's undo step, so a first Ctrl+Z does not take the
    /// seed back. A member who creates the same record concurrently
    /// seeds it too, so a seed sets values rather than adding rows.
    fn seed(cx: &SeedCx<'_>) -> Vec<Invoke<Self::Resource>> {
        let _ = cx;
        Vec::new()
    }

    /// The records the app keeps besides the one it shows: additional
    /// workspace records and settings sections that are resource models. Each is
    /// opened with the window and published beside the app's resource, so
    /// the palette and agents change it whether or not a view has shown it,
    /// and `tilolang surface` commits its surface with the app's.
    ///
    /// ```ignore
    /// fn resources(resources: &mut Resources<'_>) {
    ///     resources.workspace_record::<WatchHistory>("watch-history").settings::<Playback>();
    /// }
    /// ```
    fn resources(resources: &mut super::Resources<'_>) {
        let _ = resources;
    }

    /// The records the app keeps beside the one it shows, declared with the
    /// app rather than with the window showing it
    /// ([`PersonKind`](super::PersonKind)): each is opened with the window and
    /// part of the app's committed surface whether or not a view has shown it.
    /// A shared one ([`PersonKind::shared`](super::PersonKind::shared)) is a
    /// record one app keeps for every app of the workspace; a profile one
    /// ([`PersonKind::profile`](super::PersonKind::profile)) is the person's
    /// own, kept in their own scope instead of a workspace's.
    ///
    /// ```ignore
    /// const PERSON_RESOURCES: &'static [PersonKind] =
    ///     &[PersonKind::shared::<accounts::person::AccountList>()];
    /// ```
    const PERSON_RESOURCES: &'static [super::PersonKind] = &[];

    /// Whether a window of this app is the person's own
    /// ([`crate::personal`]): it shows what the person keeps for themselves,
    /// not the workspace's record, so it never stands on a workspace's shared
    /// desktop and comes back on the person's own shell. Declared with the
    /// app, which the fleet records, since the desktop that draws the
    /// workspace's shared one opens none of the app's code.
    ///
    /// ```ignore
    /// const WINDOWS_ARE_PERSONAL: bool = true;
    /// ```
    const WINDOWS_ARE_PERSONAL: bool = false;

    /// The app's windows on a platform this one does not serve, shown by another `ResourceApp`
    /// under the same app id (a page's, where this one is a computer's): the resources those
    /// windows keep are part of the app's committed surface too, so a workspace takes their
    /// changes and hands them to everyone else who opens one.
    ///
    /// ```ignore
    /// const OTHER_WINDOWS: &'static [OtherWindow] = &[OtherWindow::of::<PageShell>()];
    /// ```
    const OTHER_WINDOWS: &'static [OtherWindow] = &[];
}

/// A window of the same app shown by another [`ResourceApp`] ([`ResourceApp::OTHER_WINDOWS`]):
/// what its surface report says, merged into the app's.
#[derive(Clone, Copy)]
pub struct OtherWindow {
    #[cfg(not(target_arch = "wasm32"))]
    report: fn() -> serde_json::Value,
}

impl OtherWindow {
    /// The windows `B` shows.
    #[must_use]
    pub const fn of<B: ResourceApp>() -> Self {
        Self {
            #[cfg(not(target_arch = "wasm32"))]
            report: super::headless::surface_report::<B>,
        }
    }

    /// Take their surface report's resources (one of a kind `report` has not), effects, views
    /// and unbound controls into `report`, the app's own, as `tilolang surface` reads it.
    #[cfg(not(target_arch = "wasm32"))]
    pub(super) fn merge_into(self, report: &mut serde_json::Value) {
        use serde_json::Value;
        let other = (self.report)();
        let kind = |resource: &Value| resource.get("kind").cloned();
        let kinds: Vec<Option<Value>> = report
            .get("resources")
            .and_then(Value::as_array)
            .map(|resources| resources.iter().map(kind).collect())
            .unwrap_or_default();
        // Each resource keeps the owner the other window's report gave it, so a
        // resource of another app stays a read of this app's surface.
        if let (Some(theirs), Some(ours)) = (
            other.get("owners").and_then(Value::as_object),
            report.get_mut("owners").and_then(Value::as_object_mut),
        ) {
            for (kind, owner) in theirs {
                ours.entry(kind.clone()).or_insert(owner.clone());
            }
        }
        for list in ["resources", "effects", "views", "unbound"] {
            let (Some(theirs), Some(ours)) = (
                other.get(list).and_then(Value::as_array),
                report.get_mut(list).and_then(Value::as_array_mut),
            ) else {
                continue;
            };
            for item in theirs {
                let known = if list == "resources" {
                    kinds.contains(&kind(item))
                } else {
                    ours.contains(item)
                };
                if !known {
                    ours.push(item.clone());
                }
            }
        }
    }
}

/// What [`ResourceApp::opening`] builds with: who the window is for, the workspace, and the record
/// on its way.
pub struct OpeningCx<'a> {
    pub(crate) me: &'a MemberId,
    pub(crate) workspace: String,
    pub(crate) files: ui_core::storage::AppFiles,
}

impl OpeningCx<'_> {
    /// Who the window is for.
    #[must_use]
    pub fn me(&self) -> &MemberId {
        self.me
    }

    /// The workspace the window shows, as [`ResourceEnv::workspace`](super::ResourceEnv::workspace) names it.
    #[must_use]
    pub fn workspace(&self) -> &str {
        &self.workspace
    }

    /// The app's private file areas, as [`ResourceEnv::storage`](super::ResourceEnv::storage) gives
    /// them to the view.
    #[must_use]
    pub fn files(&self) -> ui_core::storage::AppFiles {
        self.files.clone()
    }
}

/// What a window shows before its record ([`ResourceApp::opening`]): a widget that the view
/// takes over as the service it was lent.
pub trait OpeningView: tilo_ui_base::widget::Widget {
    /// This, as the service lent to the view.
    fn into_lent(self: Box<Self>) -> Box<dyn std::any::Any>;
}

impl<W: tilo_ui_base::widget::Widget + 'static> OpeningView for W {
    fn into_lent(self: Box<Self>) -> Box<dyn std::any::Any> {
        self
    }
}

/// The History scope of [`ResourceApp::FOLDER_HISTORY`]: the folder a window
/// shows, every file's changes in it as one list.
#[derive(Clone, Copy)]
pub struct FolderHistory {
    /// What the scope is called in History: "Folder".
    pub title: &'static str,
    /// Whether the window's own record keeps what people did (bookmarks, settings), listed
    /// as a scope of its own beside the folder's. A window whose record keeps only which
    /// folder is open has none worth listing.
    pub beside_resource: bool,
}

/// What [`ResourceApp::seed`] may read: who is creating the record, and the
/// app's host — its settings, files and secrets on this device — when the
/// record is created in a window. A member without one (an agent, a
/// workspace service: [`seed_resource`](super::seed_resource)) creates it with no
/// host: what a seed imports from the device is the device's own to seed.
pub struct SeedCx<'a> {
    pub(crate) me: &'a MemberId,
    pub(crate) device: &'a str,
    pub(crate) host: Option<&'a ProcessContext>,
}

impl SeedCx<'_> {
    /// Who is creating the record: the id their views read as
    /// [`ResourceEnv::me`](super::ResourceEnv::me) and the seed's actions read as their author.
    pub fn me(&self) -> &MemberId {
        self.me
    }

    /// The device creating the record.
    pub fn device(&self) -> &str {
        self.device
    }

    /// The app's host on this device; `None` for a member without a
    /// window.
    pub fn host(&self) -> Option<&ProcessContext> {
        self.host
    }
}

type Leaked = &'static (dyn std::any::Any + Send + Sync);

pub(crate) fn leaked<T: Send + Sync + 'static>(
    key: TypeId,
    make: impl FnOnce() -> T,
) -> &'static T {
    let mut make = Some(make);
    let entry = leaked_entry(key, &mut || match make.take() {
        Some(make) => Box::leak(Box::new(make())) as Leaked,
        None => unreachable_definition(),
    });
    // One value per (type, T) key is ever inserted, so the downcast holds.
    entry
        .downcast_ref::<T>()
        .unwrap_or_else(|| unreachable_definition())
}

/// The value kept under `key`, made by `make` the first time. Not generic,
/// so the table is compiled once rather than for every app and key.
#[inline(never)]
fn leaked_entry(key: TypeId, make: &mut dyn FnMut() -> Leaked) -> Leaked {
    static TABLE: OnceLock<Mutex<HashMap<TypeId, Leaked>>> = OnceLock::new();
    let table = TABLE.get_or_init(Mutex::default);
    let mut table = table.lock_recover();
    *table.entry(key).or_insert_with(make)
}

#[allow(
    clippy::panic,
    reason = "a keyed table entry of the wrong type is a framework bug"
)]
fn unreachable_definition() -> ! {
    panic!("framework definition table holds a value of another type")
}

struct DefinitionKey<A>(std::marker::PhantomData<A>);

/// The toolkit application definition of `A`, created once per process.
pub fn definition<A: ResourceApp>() -> &'static ApplicationDefinition {
    leaked(TypeId::of::<DefinitionKey<A>>(), || {
        super::kinds::register::<A>();
        definition_of::<A>()
    })
}

fn definition_of<A: ResourceApp>() -> ApplicationDefinition {
    // An app's library and settings are one record per workspace: every
    // window of it there opens the same data, across closing, reopening and
    // restarts, and keeps only its view of it (scroll, selection). An app
    // whose windows each show a resource of their own (a document, a
    // session) keeps that per window.
    application_definition(hooks::<A>(), A::OPENS, !A::WINDOW_RESOURCE, create::<A>)
}

/// The toolkit definition of the app `app` describes. Not generic, so it is
/// compiled once rather than for every app.
#[inline(never)]
fn application_definition(
    app: &AppHooks,
    opens: &'static [&'static str],
    workspace_resource: bool,
    create: ComponentFactory,
) -> ApplicationDefinition {
    ApplicationDefinition {
        app_id: app.id().into(),
        title: app.title().into(),
        icon: icon(app),
        resource_type: ResourceType::new(app.kind),
        required: vec![],
        optional: vec![],
        opens,
        workspace_resource,
        opens_before_resource: workspace_resource && app.opens_before_resource,
        create,
    }
}

fn icon(app: &AppHooks) -> IconDescriptor {
    IconDescriptor::Named(
        app.icon_name()
            .unwrap_or_else(|| app.id().rsplit('.').next().unwrap_or_else(|| app.id()))
            .to_owned(),
    )
}

/// What the launch path, the window and History read of an app: its
/// constants, and shims over the parts that need its types. One table per
/// app; everything that reads it is compiled once for every app.
#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag is one of the app's own constants, read as it declares it"
)]
pub(crate) struct AppHooks {
    id: &'static str,
    /// [`ResourceApp::title`], per app id so the answer is resolved once.
    title: fn(&'static str) -> &'static str,
    icon: Option<&'static str>,
    /// [`ResourceApp::named`] — the app's name when the launch gives it.
    named: fn() -> Option<AppName>,
    /// [`Resource::KIND`] of the app's resource.
    pub(crate) kind: &'static str,
    /// [`Resource::LABEL`] of the app's resource.
    label: &'static str,
    behaviour: ComponentBehaviour,
    document_history: bool,
    folder_history: Option<FolderHistory>,
    #[cfg(tilo_native_runner)]
    private_windows: bool,
    /// [`ResourceApp::ATTACHES_RUNTIME`].
    #[cfg(tilo_native_runner)]
    attaches_runtime: bool,
    #[cfg(tilo_native_runner)]
    options: &'static [&'static str],
    #[cfg(tilo_native_runner)]
    flags: &'static [&'static str],
    catalog: fn() -> Option<&'static EmbeddedCatalog>,
    definition: fn() -> &'static ApplicationDefinition,
    launch_resource: fn(&LaunchInput<'_>) -> Option<DocumentId>,
    local_operations: fn() -> Vec<tilo_actions::ActionSpec>,
    declarations: fn() -> Rc<[tilo_resource::Declaration]>,
    /// Initialize a record the launch created: the resource's own
    /// initialization, the launch's genesis and the creator's seed.
    new_resource: NewResource,
    prepare_capture: fn(&mut ProcessContext, Option<&str>) -> Result<Run, LaunchError>,
    /// Read the record `context` opened, build the root over it and take
    /// the launch's actions on it; or why the record does not read.
    open: OpenRoot,
    /// [`ResourceApp::OPENS_BEFORE_RESOURCE`].
    opens_before_resource: bool,
    /// [`ResourceApp::opening`].
    pub(crate) opening: fn(&OpeningCx<'_>) -> Option<Box<dyn OpeningView>>,
    pub(crate) action_registry: fn() -> tilo_actions::ActionRegistry,
    history: fn() -> &'static ui_core::history_mode::HistoryDefinition,
    folder_history_observation: fn() -> &'static ui_core::history_mode::HistoryDefinition,
}

/// [`AppHooks::new_resource`] — the context, and what the launch named.
type NewResource = fn(
    &mut ProcessContext,
    &[String],
    Option<&ui_core::application::OpenFile>,
    &[ui_core::application::LaunchOption],
) -> Result<(), LaunchError>;

/// [`AppHooks::open`] — the context, who edits, whether it is a capture and
/// whether the launch created the record.
/// The window's root, and the parts whose versions its History shows and restores.
type OpenedRoot = (
    Box<dyn ui_core::widget::AppWidget>,
    Rc<dyn ui_core::history_mode::HistoryParts>,
);

type OpenRoot =
    fn(&mut ProcessContext, Editing, bool, bool) -> Result<Result<OpenedRoot, String>, LaunchError>;

struct Hooks<A>(std::marker::PhantomData<A>);

impl<A: ResourceApp> Hooks<A> {
    const HOOKS: AppHooks = AppHooks {
        id: A::ID,
        title: titled::<A>,
        icon: A::ICON,
        named: A::named,
        kind: <A::Resource as Resource>::KIND,
        label: <A::Resource as Resource>::LABEL,
        behaviour: A::BEHAVIOUR,
        document_history: A::DOCUMENT_HISTORY,
        folder_history: A::FOLDER_HISTORY,
        #[cfg(tilo_native_runner)]
        private_windows: A::PRIVATE_WINDOWS,
        #[cfg(tilo_native_runner)]
        attaches_runtime: A::ATTACHES_RUNTIME,
        #[cfg(tilo_native_runner)]
        options: A::OPTIONS,
        #[cfg(tilo_native_runner)]
        flags: A::FLAGS,
        catalog: A::catalog,
        definition: definition::<A>,
        launch_resource: A::launch_resource,
        local_operations: A::local_operations,
        declarations: declarations_of::<A::Resource>,
        new_resource: new_resource::<A>,
        prepare_capture: prepare_capture::<A>,
        open: open_root::<A>,
        opens_before_resource: A::OPENS_BEFORE_RESOURCE && !A::WINDOW_RESOURCE,
        opening: A::opening,
        action_registry: action_registry::<A>,
        history: super::preview::history_definition::<A>,
        folder_history_observation: super::preview::folder_history_observation::<A>,
    };
}

impl AppHooks {
    /// The app's id: as the launch names it, else [`ResourceApp::ID`].
    pub(crate) fn id(&self) -> &'static str {
        (self.named)().map_or(self.id, |named| named.id)
    }

    /// The window and launcher title: as the launch names it, else the app's
    /// catalogue's name for it, resolved once per process — every host holds
    /// a definition for as long.
    pub(crate) fn title(&self) -> &'static str {
        match (self.named)() {
            Some(named) => named.title,
            None => (self.title)(self.id()),
        }
    }

    fn icon_name(&self) -> Option<&'static str> {
        (self.named)().map_or(self.icon, |named| named.icon)
    }
}

/// `A`'s [`AppHooks`].
pub(crate) fn hooks<A: ResourceApp>() -> &'static AppHooks {
    &Hooks::<A>::HOOKS
}

/// Every app's title, resolved from its catalogue once per language: a host
/// keeps its definitions for the process's life, so a title is worth keeping
/// for as long.
static TITLES: Mutex<Vec<(&'static str, String, &'static str)>> = Mutex::new(Vec::new());

/// [`ResourceApp::TITLE_KEY`] for the app `id`, in the language the person
/// reads, else the source-language [`ResourceApp::TITLE`]. One place resolves
/// every app's title, so no app can ship an English-only one.
fn titled<A: ResourceApp>(id: &'static str) -> &'static str {
    let (Some(catalog), false) = (A::catalog(), A::TITLE_KEY.is_empty()) else {
        return A::TITLE;
    };
    let locale = catalog.current_locale();
    let mut titles = TITLES.lock_recover();
    if let Some((_, _, title)) = titles
        .iter()
        .find(|(app, kept, _)| *app == id && *kept == locale)
    {
        return title;
    }
    let title: &'static str = Box::leak(catalog.t(A::TITLE_KEY).into_boxed_str());
    titles.push((id, locale, title));
    title
}

fn declarations_of<R: Resource>() -> Rc<[tilo_resource::Declaration]> {
    tilo_resource::vocabulary::<R>().declarations()
}

/// Who may edit the resource a root shows, and what it is built for.
pub(crate) enum Editing {
    /// The member (`me`) and their permissions, followed while the app
    /// runs, in a window someone uses or a capture.
    Live {
        permissions: LivePermissions,
        me: MemberId,
        run: Box<Run>,
    },
    /// Never: a past version in History mode, launched as `run` says (the record whose chains it
    /// reads, the item it shows).
    ReadOnly(Box<Run>),
}

/// The permission that makes the record `run` opened editable: writing the
/// app's records of the workspace, or, for a record of a kind each person keeps
/// their own of, writing that record, which the workspace grants its person.
fn write_scope(app: &AppHooks, run: &Run) -> PermissionScope {
    let own_kind = tilo_actions::policy::roles::has_member_reader(app.id(), app.kind);
    match run
        .launched
        .record
        .as_ref()
        .map(ui_core::resource::DocumentId::route)
    {
        Some(record) if own_kind => PermissionScope::Action {
            node: tilo_ui_base::permissions::TreePath::root()
                .app(app.id())
                .kind(app.kind)
                .resource(record),
            kind: ActionKind::Undoable,
        },
        _ => PermissionScope::app(app.id(), ActionKind::Undoable),
    }
}

/// What a root is built with besides its resource type: who edits it and
/// how, what it was built for, and where its settings are kept.
struct RootShell {
    /// The permission that makes the app's resource editable.
    scope: PermissionScope,
    permissions: Option<LivePermissions>,
    read_only: bool,
    me: Option<MemberId>,
    run: Run,
    storage: Rc<super::settings::ViewStorage>,
}

impl RootShell {
    fn new(
        app: &AppHooks,
        editing: Editing,
        storage: AppStorage,
        hosted: super::settings::Hosted,
    ) -> Self {
        let (permissions, read_only, me, run) = match editing {
            Editing::Live {
                permissions,
                me,
                run,
            } => (Some(permissions), false, Some(me), *run),
            Editing::ReadOnly(run) => (None, true, None, *run),
        };
        let scope = write_scope(app, &run);
        let read_only = read_only
            || permissions
                .as_ref()
                .is_some_and(|permissions| !permissions.check(&scope));
        let run = run.for_member(me.as_ref());
        let storage =
            super::settings::ViewStorage::new(storage, hosted.attach(app.id(), app.title()));
        Self {
            scope,
            permissions,
            read_only,
            me,
            run,
            storage,
        }
    }

    /// Whether effects and projection drains run: in a window someone uses
    /// only. History shows a past version, and a capture reaches nothing
    /// outside.
    fn runs_effects(&self) -> bool {
        self.permissions.is_some() && self.run.mode.is_live()
    }

    /// Whether the effects someone invokes run, though the window starts
    /// no work on its own: a capture a headless drive invokes actions on.
    fn runs_invoked_effects(&self) -> bool {
        self.permissions.is_some() && self.run.driven
    }
}

/// [`ResourceApp::label`] of `state`, the name the app gives the resource as
/// it stands: what a root publishes whenever it publishes a state, so the
/// name follows the resource (a library of one book is "1 book" as soon as
/// the book is shelved) rather than the state the window opened on.
fn live_label<A: ResourceApp>(state: &dyn std::any::Any) -> String {
    state
        .downcast_ref::<<A::Resource as Resource>::State>()
        .map_or_else(<A::Resource as Resource>::label, A::label)
}

/// Build the framework root for a resource state: the live handle, the
/// dispatcher, the published resource and the app's view.
pub(crate) fn build_root<A: ResourceApp>(
    reader: Reader<A::Resource>,
    editing: Editing,
    storage: AppStorage,
    hosted: super::settings::Hosted,
) -> ResourceRoot {
    let app = hooks::<A>();
    let share = super::share_link::ShareLinks::default();
    // History names this app's fields by their titles.
    if let Some(shape) = <A::Resource as Resource>::shape() {
        ui_core::history_source::sentence::learn_field_titles(&shape);
    }
    let shell = RootShell::new(app, editing, storage, hosted);
    let runs_effects = shell.runs_effects();
    let runs_invoked_effects = shell.runs_invoked_effects();
    let RootShell {
        scope,
        permissions,
        read_only,
        me,
        run,
        storage,
    } = shell;
    // The folder the window's History lists is the workspace's tree, opened with the
    // window's storage: the record the window's rows read.
    if let Some((folder, _)) = &run.folder_history {
        super::folder::show(folder, &storage, &run.people);
    }
    let label = A::label(reader.state());
    let published = tilo_resource::publish::<A::Resource>(app.kind, &label, reader.state());
    let live = Live::new(reader);
    let alive = Rc::new(());
    let dispatch = Dispatcher::new(app.id(), app.kind, read_only, me.as_ref()).on_resource(
        run.launched
            .record
            .as_ref()
            .map_or("", |record| record.route()),
    );
    let dispatch = if permissions.is_none() {
        dispatch.past()
    } else {
        dispatch
    };
    // The name the app gives the record it opened: the first publication
    // names it by the state it holds then, and every one after by its own.
    dispatch.name_resource(&label);
    // A window someone uses opens the records the app keeps beside its own,
    // so they are published before any view shows them.
    if permissions.is_some() {
        A::resources(
            &mut super::Resources::new(&storage)
                .with_base(live.clone(), dispatch.clone())
                .at(run.launched.record.as_ref()),
        );
        for record in A::PERSON_RESOURCES {
            record.open(&storage);
        }
    }
    // What the declarations opened shows once it arrived, also what is on its way from
    // another member's device: the view is built once, and built before it would read that
    // the workspace has none of it.
    let declared = storage.declared_arriving();
    let shows = run.launched.shows.clone();
    let effects = if runs_effects {
        Some(Effects::live(
            dispatch.clone(),
            live.clone(),
            shows,
            A::OPENS,
        ))
    } else {
        runs_invoked_effects
            .then(|| Effects::invoked(dispatch.clone(), live.clone(), shows, A::OPENS))
    };
    let awaiting = run.workspace.clone();
    let links = dispatch.clone();
    let open_link: super::root::OpenLink = Rc::new(move |link: &str| {
        let invokes =
            A::open_link(link).ok_or_else(|| format!("{} does not open {link}", A::ID))?;
        for invoke in invokes {
            links.submit(invoke);
        }
        Ok(())
    });
    let mut cx = ViewCx::root(live, &alive, dispatch, published, run, storage);
    if let Some(effects) = effects {
        cx.env.effects = effects;
    }
    // A record the workspace may hold shows once the workspace's holdings arrived;
    // the app's view says nothing about it. Holdings already here (no
    // shell hosts the window, or the workspace synced) need no stand-in.
    let content = arrived_content::<A>(&mut cx, awaiting, declared);
    let root = ResourceRoot::new(
        (app.title(), icon(app), live_label::<A>),
        cx,
        alive,
        content,
        share,
    );
    #[cfg(not(target_family = "wasm"))]
    let past: Option<crate::runner::DrawPast> = Some(super::preview::draw_past::<A>);
    #[cfg(target_family = "wasm")]
    let past = None;
    finish_root(
        root,
        app,
        (permissions, scope),
        open_link,
        A::BEHAVIOUR.window,
        past,
    )
}

/// What a window over the past version of an app's record draws it with.
#[cfg(not(target_family = "wasm"))]
type PastDrawing = crate::runner::DrawPast;
/// A browser window shows no past version of its own.
#[cfg(target_family = "wasm")]
type PastDrawing = ();

/// [`build_root`]'s root once its view is built: following `permissions`
/// for `scope`, opening links and keeping its record's index entry, painted
/// as an app's window of the role `window` paints and drawing a past version
/// with `past` when someone uses it. Not generic, so it is compiled once
/// rather than in every app.
#[inline(never)]
fn finish_root(
    root: ResourceRoot,
    app: &AppHooks,
    (permissions, scope): (Option<LivePermissions>, PermissionScope),
    open_link: super::root::OpenLink,
    window: ui_core::application::WindowRole,
    past: Option<PastDrawing>,
) -> ResourceRoot {
    let live = permissions.is_some();
    let mut root = root
        .following(permissions, scope)
        .with_presentation((app.local_operations)())
        .opening_links(open_link);
    // A window shown over other apps' content paints no ground of its own:
    // what it does not paint shows what is under it, and a keyboard that
    // hides its keys takes the whole of its window with them.
    if matches!(
        window,
        ui_core::application::WindowRole::Sheet | ui_core::application::WindowRole::InputSurface
    ) {
        root = root.without_ground();
    }
    if !app.document_history {
        return root.without_history();
    }
    #[cfg(not(target_family = "wasm"))]
    if let Some(past) = past.filter(|_| live) {
        return root.drawing_past(past);
    }
    #[cfg(target_family = "wasm")]
    let _ = (live, past);
    root
}

/// The workspace's holdings, as a window waits for them.
type WorkspaceArrival =
    tilo_ui_base::arriving::Arriving<(), tilo_ui_base::workspace_sync::WorkspaceUnreachable>;

/// The documents a window's app declared, as the window waits for them.
type DeclaredArrival =
    tilo_ui_base::arriving::Arriving<(), tilo_ui_base::workspace_sync::RecordNotHere>;

/// The window's view, once what it waits for arrived: the workspace's holdings and the
/// documents the app's declarations opened.
fn arrived_content<A: ResourceApp>(
    cx: &mut ViewCx<A::Resource>,
    holdings: Option<WorkspaceArrival>,
    declared: Option<DeclaredArrival>,
) -> Element {
    match (holdings, declared) {
        (Some(holdings), declared) if workspace_pending(&holdings) => {
            cx.arriving_workspace(&holdings, move |cx| match &declared {
                Some(declared) => cx.arriving_workspace(declared, A::view),
                None => A::view(cx),
            })
        }
        (_, Some(declared)) => cx.arriving_workspace(&declared, A::view),
        (_, None) => A::view(cx),
    }
}

/// Whether the workspace's holdings are still on their way.
fn workspace_pending(
    holdings: &tilo_ui_base::arriving::Arriving<
        (),
        tilo_ui_base::workspace_sync::WorkspaceUnreachable,
    >,
) -> bool {
    holdings.with(|workspace| workspace.ready().is_none())
}

/// The decoders of every edit `A`'s document takes: its resource's actions
/// and History's restores.
pub(crate) fn action_registry<A: ResourceApp>() -> tilo_actions::ActionRegistry {
    let mut registry =
        tilo_actions::ActionRegistry::with(tilo_resource::group::register::<A::Resource>);
    registry
        .register::<super::history::ResourceHistoryEdit<A::Resource>>(super::history::EDIT_TYPE);
    registry
}

/// The actions `invokes` build at `now_ms`, groups expanded; or the first
/// refusal.
fn built_actions<R: Resource>(
    invokes: &[Invoke<R>],
    now_ms: i64,
) -> Result<Vec<R::Action>, String> {
    let mut actions = Vec::new();
    for invoke in invokes {
        if let Some(refusal) = invoke.refusal() {
            return Err(refusal.to_owned());
        }
        if invoke.is_group() {
            actions
                .extend(tilo_resource::group::build_group::<R>(&invoke.arguments, now_ms)?.actions);
        } else {
            actions.push(invoke.build(now_ms)?);
        }
    }
    Ok(actions)
}

/// Apply a new record's [`ResourceApp::seed`] through the replayable
/// executor: attributed to `author` as their edits are, and recorded as
/// content from outside, which no undo takes back.
pub(crate) fn apply_seed<A: ResourceApp>(
    doc: &mut tilo_resource::Doc,
    author: &MemberId,
    seed: &[Invoke<A::Resource>],
) -> Result<(), String> {
    let actions = built_actions(seed, tilo_resource::origin_now_ms())?;
    let encoded = actions
        .iter()
        .map(|action| tilo_resource::encode::<A::Resource>(action).map_err(|e| e.to_string()))
        .collect::<Result<Vec<_>, _>>()?;
    let actions: Vec<&dyn tilo_actions::Action> = actions
        .iter()
        .map(|action| action as &dyn tilo_actions::Action)
        .collect();
    apply_seed_actions(hooks::<A>(), doc, author, &actions, encoded)
}

/// [`apply_seed`] once the seed is built and encoded.
fn apply_seed_actions(
    app: &AppHooks,
    doc: &mut tilo_resource::Doc,
    author: &MemberId,
    actions: &[&dyn tilo_actions::Action],
    encoded: Vec<tilo_actions::EncodedAction>,
) -> Result<(), String> {
    let writer =
        ui_core::history_source::authors::device_actor(&author.actor_bytes(), doc.get_actor());
    for (action, encoded) in actions.iter().zip(encoded) {
        let context = tilo_actions::ActionContext::for_resource(app.kind, app.kind, app.label)
            .with_app(app.id(), app.title())
            .with_invoker(tilo_actions::ActionInvoker::member(author.clone()))
            .with_change(tilo_actions::ChangeKind::External);
        tilo_actions::apply_local_with_context(doc, *action, encoded, Some(&writer), context)
            .map_err(|error| format!("seeding the new record failed: {error}"))?;
    }
    Ok(())
}

/// Write a launch's [`ResourceApp::genesis`] into the record's genesis:
/// built at time zero and applied as the genesis writes everything, so every
/// device that creates the record writes the same change.
fn write_genesis<R: Resource>(
    doc: &mut tilo_resource::Doc,
    genesis: &[Invoke<R>],
) -> Result<(), String> {
    for action in &built_actions(genesis, 0)? {
        tilo_actions::Action::apply(action, doc).map_err(|error| error.to_string())?;
    }
    Ok(())
}

/// Initialize a record a launch of `A` created: the resource's own
/// initialization and the launch's [`ResourceApp::genesis`] in the
/// genesis, then the creator's [`ResourceApp::seed`].
fn new_resource<A: ResourceApp>(
    context: &mut ProcessContext,
    arguments: &[String],
    file: Option<&ui_core::application::OpenFile>,
    options: &[ui_core::application::LaunchOption],
) -> Result<(), LaunchError> {
    let genesis = A::genesis(
        &LaunchInput::new(arguments, file)
            .with_options(options)
            .with_content(context.content()),
    );
    let author = context.author().clone();
    let seed = A::seed(&SeedCx {
        me: &author,
        device: context.device_id(),
        host: Some(context),
    });
    initialize_resource(
        context,
        &mut |doc| {
            <A::Resource as Resource>::initialize(doc).map_err(|error| error.to_string())?;
            write_genesis(doc, &genesis)
        },
        &mut |doc| apply_seed::<A>(doc, &author, &seed),
    )
}

/// What initializing a new record writes into it.
type ResourceWrite<'a> = &'a mut dyn FnMut(&mut tilo_resource::Doc) -> Result<(), String>;

/// Initialize the new record `context` opened: `genesis` into its genesis, then `seed`. Not
/// generic over the app, so the record store's initialization is compiled once.
fn initialize_resource(
    context: &mut ProcessContext,
    genesis: ResourceWrite<'_>,
    seed: ResourceWrite<'_>,
) -> Result<(), LaunchError> {
    context
        .initialize_new_resource_seeded(|doc| genesis(doc), |doc| seed(doc))
        .map_err(LaunchError::Record)
}

/// Bring an older record up to the resource's storage version and this
/// build's version key; what it answers when it cannot. Only a document that
/// needs it is written to: opening another view of a current record must not
/// commit what one already open has pending.
fn migrate_older(
    context: &ProcessContext,
    needs: fn(&tilo_resource::Doc) -> bool,
    migrate: &mut dyn FnMut(&mut tilo_resource::Doc) -> Result<bool, String>,
) -> Result<Result<(), String>, LaunchError> {
    let older = context.read_resource(needs).map_err(LaunchError::Record)?;
    Ok(if older {
        context.migrate_resource(|doc| migrate(doc)).map(drop)
    } else {
        Ok(())
    })
}

/// Read the record `context` opened as `A`'s resource, once an older
/// document is brought up to the app's storage version, build its root and
/// take the launch's actions ([`ResourceApp::launch`]) on it; `created`
/// when the launch created the record. Or why it does not read, which the
/// window shows instead of the view.
///
/// A capture launched on nothing is staged by its scenario alone: what a
/// launch on nothing takes reads from this device (a library, the last
/// folder), which a screenshot must not depend on. A capture launched on a
/// file takes that file, as the command line does.
fn open_root<A: ResourceApp>(
    context: &mut ProcessContext,
    editing: Editing,
    capture: bool,
    created: bool,
) -> Result<Result<OpenedRoot, String>, LaunchError> {
    let id = context.record().id.route().to_owned();
    let migrated = migrate_older(
        context,
        tilo_resource::evolution::needs_migrate::<A::Resource>,
        &mut |doc| {
            tilo_resource::founding::bring_up_resource::<A::Resource>(&id, doc)
                .map_err(|error| error.to_string())
        },
    )?;
    let read = context
        .read_resource(|doc| {
            Reader::<A::Resource>::read(doc)
                .map_err(|error| tilo_resource::Unreadable::of::<A::Resource>(doc, &error).reason)
        })
        .map_err(LaunchError::Record)?;
    let reader = match migrated.and(read) {
        Ok(reader) => reader,
        Err(reason) => return Ok(Err(reason)),
    };
    let record = match &editing {
        Editing::Live { run, .. } => run.launched.record.clone(),
        Editing::ReadOnly(_) => None,
    };
    let root = build_root::<A>(reader, editing, context.storage(), hosted(context));
    if let Some(typed) = root.typed::<A::Resource>() {
        let record = typed.live.get();
        let launch = root.launch().with_resource(&*record);
        if !capture || !launch.is_empty() {
            // A launch naming a link the app opens shows it as `open-link` does.
            let invokes = launch
                .argument()
                .filter(|argument| {
                    A::OPENS_LAUNCH_LINK && tilo_actions::remote::link_scheme(argument).is_some()
                })
                .and_then(A::open_link)
                .unwrap_or_else(|| A::launch(&launch, created));
            for invoke in invokes {
                typed.dispatch.submit(invoke);
            }
        }
    }
    let parts = Rc::new(super::history::ChainHistory {
        storage: Rc::downgrade(&root.storage),
        preview: super::preview::part_preview::<A>,
        record,
    });
    Ok(Ok((root.into_widget(), parts)))
}

/// The component of a window shown before its record ([`ResourceApp::opening`]): it shows
/// `view` until the record is had, then the window shows the component built as any launch
/// builds it, which takes `view` over ([`super::Opened`]).
fn opening_component(
    app: &'static AppHooks,
    mut context: ProcessContext,
    view: Box<dyn OpeningView>,
    record: ui_core::resource::ResourceOpening,
    restore: &RestoreInput,
) -> AppComponent {
    let arguments = context.take_launch_arguments();
    let options = context.take_launch_options();
    let handoff = context.take_handoff();
    let lent = context.take_lent();
    let restore = restore.clone();
    let reopener = context.reopener();
    let checkout = record.checkout().clone();
    let build: super::opening::Build = Box::new(move |shown| {
        let definition = (app.definition)();
        let (mut context, target) = reopener.prepare(definition, checkout.id)?;
        context.provide_launch_arguments(arguments.clone());
        context.provide_launch_options(options);
        if let Some(handoff) = handoff {
            context.provide_handoff(handoff);
        }
        for service in lent {
            context.lend_boxed(service);
        }
        context.lend(super::Opened::new(shown));
        let component = definition.instantiate(context, target, restore)?;
        Ok(ui_core::application::ComponentReplacement::new(
            component, arguments,
        ))
    });
    let root = super::opening::OpeningRoot::new(view, record, build, (app.title(), icon(app)));
    let mut component = AppComponent::new(context, Box::new(root), (app.action_registry)());
    component.set_behaviour(app.behaviour);
    component
}

/// What a window's root keeps of the person and the device.
pub(crate) fn hosted(context: &ProcessContext) -> super::settings::Hosted {
    super::settings::Hosted {
        // A private window keeps nothing of the person's: their own
        // records are not even opened.
        person: (!context.is_private())
            .then(|| context.person_resources().cloned())
            .flatten(),
        device: Some(context.device_id().to_owned()),
        records: Some((
            context.records().clone(),
            context.author().clone(),
            Some(context.live_permissions()),
        )),
        workspace: context
            .workspace_resources()
            .cloned()
            .map(|records| (records, context.wake_handle())),
        wake: Some(context.wake_handle()),
    }
}

/// The window of a record that does not read as the app's resource.
fn unopened(app: &AppHooks, context: ProcessContext, reason: &str) -> AppComponent {
    let unopened = super::root::Unopened::new(
        app.title(),
        icon(app),
        (app.kind, app.label),
        &(app.declarations)(),
        reason,
    );
    let mut component = AppComponent::new(context, unopened.boxed(), (app.action_registry)());
    component.set_behaviour(app.behaviour);
    // An earlier version of the record may still read as the resource:
    // History shows its versions and restores one, as in the app's own window. A
    // folder's History needs the folder itself, which a notice has not opened.
    if app.folder_history.is_none()
        && app.document_history
        && let Err(error) = component.register_document_history((app.history)())
    {
        tracing::warn!(%error, "the unopened record shows without its History");
    }
    component
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "the toolkit's ComponentFactory signature passes the target owned"
)]
fn create<A: ResourceApp>(
    context: ProcessContext,
    target: LaunchTarget,
    restore: RestoreInput,
) -> Result<AppComponent, LaunchError> {
    // The window's record is founded by the type the app holds of it, the same on every device.
    context.records().declare_type(
        <A::Resource as Resource>::KIND,
        <A::Resource as Resource>::surface,
    );
    create_component(hooks::<A>(), context, &target, &restore)
}

fn create_component(
    app: &'static AppHooks,
    mut context: ProcessContext,
    target: &LaunchTarget,
    restore: &RestoreInput,
) -> Result<AppComponent, LaunchError> {
    if let Some(catalog) = (app.catalog)() {
        catalog.ensure();
    } else {
        // A window with no catalogue of its own — a sheet of the frames, a system menu — still
        // speaks the person's language: the toolkit's own labels are part of every window, and on
        // a thread with no catalogue yet they install it in the desktop's language.
        tilo_ui_base::i18n::I18nManager::register_widget_messages();
    }
    // The window's record is being read off the frame: the window shows what the app shows
    // before it, and opens as below once the record is had.
    if let Some(record) = context.resource_opening() {
        let opening = OpeningCx {
            me: context.author(),
            workspace: context.records().home(),
            files: context.files(),
        };
        if let Some(view) = (app.opening)(&opening) {
            return Ok(opening_component(app, context, view, record, restore));
        }
        // An app that shows nothing before its record waits for it to be decoded; while the
        // workspace has not answered for it, the window shows the shape of the page on its way.
        // One the workspace answers it holds none of is made as the window's first frame
        // builds over it.
        if !record.take_in(true).map_err(LaunchError::Record)? {
            let arriving = Box::new(ui_core::widget::Skeleton::new(
                tilo_ui_base::widget::SkeletonShape::Page,
            ));
            return Ok(opening_component(app, context, arriving, record, restore));
        }
    }
    let starting = start_resource(app, &mut context, target)?;
    window(app, context, starting)
}

/// What the launch read of the record the window starts on, before the root
/// is built over it.
struct Starting {
    /// The recording the launch opened, when it replays one.
    replay: Option<tilo_resource::recording::Recording>,
    /// The folder the window's History lists, when the app declares one: filled once the
    /// root's storage is open.
    folder: Option<ui_core::history_mode::folder::FolderHistorySource>,
    /// Who edits, over the run the launch started.
    editing: Editing,
    /// Whether the launch created the record.
    created: bool,
    /// Whether the launch stages a capture.
    capture: bool,
}

/// Open the record the launch starts on: create or replay it, then read its
/// file, its arguments and options, and start the run they go with.
fn start_resource(
    app: &'static AppHooks,
    context: &mut ProcessContext,
    target: &LaunchTarget,
) -> Result<Starting, LaunchError> {
    // A record of its own, or the workspace's record an app keeps one of
    // (`ResourceApp::WORKSPACE_RESOURCE`), opened for the first time.
    let created = matches!(target, LaunchTarget::New | LaunchTarget::PrimaryNew);
    document_content(context)?;
    let file = context.take_open_file();
    let arguments = context.take_launch_arguments();
    let options = context.take_launch_options();
    let replay = file
        .as_ref()
        .filter(|file| file.media_type() == tilo_content::RECORDING.mime)
        .map(|file| replay_of(app, context, file, created))
        .transpose()?;
    match &replay {
        // The replay's record is the recording, whole: a private record nobody else founds,
        // whose containers are the recording's own rather than a founding beside them.
        Some(recording) => initialize_resource(context, &mut |_| Ok(()), &mut |doc| {
            let mut recorded = recording.doc().clone();
            recorded.set_actor(doc.get_actor().clone());
            *doc = recorded;
            Ok(())
        })?,
        None if created => (app.new_resource)(context, &arguments, file.as_ref(), &options)?,
        None => {}
    }
    let capture = context.capture().is_some();
    let folder = app
        .folder_history
        .map(|_| ui_core::history_mode::folder::FolderHistorySource::default());
    // A window someone uses can open another record in its place; a
    // capture shows the one it staged.
    let opener = (!capture).then(|| {
        let reopener = context.reopener();
        super::launch::OpenHere::new(move |arguments| open_in_place(app, &reopener, arguments))
    });
    let run = match context.capture().cloned() {
        Some(capture) => {
            (app.prepare_capture)(context, capture.scenario())?.driven_by_drive(capture.is_driven())
        }
        // The workspace's record, not on this device when the window opened:
        // the workspace may hold it, so the view waits for what the workspace holds.
        None => Run::live(context.content().cloned())
            .in_private(context.is_private())
            .in_embedded(context.is_embedded())
            .awaiting_workspace(
                matches!(target, LaunchTarget::PrimaryNew)
                    .then(tilo_ui_base::workspace_sync::holdings),
            ),
    };
    if let Some(file) = &file {
        file.report_opened(app.kind);
    }
    let run = run
        .launched(Launched {
            record: Some(context.record().id.clone()),
            arguments,
            options,
            file,
            handoff: context.take_handoff(),
            lent: super::scenario::Lent::new(context.take_lent()),
            reveal: std::cell::RefCell::new(
                ui_core::window_link::requested_reveal().or_else(|| revealed(target)),
            ),
            shows: Some(context.window_shows()),
        })
        .opening_with(opener)
        .showing_folder_history(
            folder
                .clone()
                .map(|source| (source, (app.folder_history_observation)())),
        );
    let editing = Editing::Live {
        permissions: context.live_permissions(),
        me: context.author().clone(),
        run: Box::new(run),
    };
    Ok(Starting {
        replay,
        folder,
        editing,
        created,
        capture,
    })
}

/// Build the window over the record the launch started on: its root, the
/// document History its versions go to, and the step a replay opens at.
fn window(
    app: &'static AppHooks,
    mut context: ProcessContext,
    starting: Starting,
) -> Result<AppComponent, LaunchError> {
    let Starting {
        replay,
        folder,
        editing,
        created,
        capture,
    } = starting;
    let (root, parts) = match (app.open)(&mut context, editing, capture, created)? {
        Ok(opened) => opened,
        Err(reason) => return Ok(unopened(app, context, &reason)),
    };
    let mut component = AppComponent::new(context, root, (app.action_registry)());
    component.set_behaviour(app.behaviour);
    if app.document_history {
        let history = (app.history)();
        match folder {
            Some(folder) => component.register_document_folder_history(history, folder),
            None => component.register_document_history(history),
        }
        .map_err(|error| LaunchError::Operation(error.to_string()))?;
        component.list_parts_in_history(&parts);
    }
    // A replay opens at the recording's first step: the state the window
    // showed when the recording starts.
    if let Some(start) = replay.as_ref().and_then(|recording| recording.heads_at(0)) {
        for hash in start {
            component
                .open_history_at(ui_core::history_source::entry_id_from_hash(
                    &hash.to_string(),
                ))
                .map_err(|error| LaunchError::Operation(error.to_string()))?;
        }
    }
    Ok(component)
}

/// The recording `file` holds, opened in a window of its own: a private
/// window on a record the launch created, which reads and is never written,
/// so the recording reaches nothing outside it
/// (`docs/architecture/session-replay.md`).
///
/// # Errors
/// An app that keeps no History, a window that is not private or not on a
/// new record, or bytes that are no recording.
fn replay_of(
    app: &AppHooks,
    context: &mut ProcessContext,
    file: &ui_core::application::OpenFile,
    created: bool,
) -> Result<tilo_resource::recording::Recording, LaunchError> {
    if !app.document_history {
        return Err(LaunchError::Operation(format!(
            "{} keeps no History to replay a recording in",
            app.title()
        )));
    }
    if !context.is_private() || !created {
        return Err(LaunchError::Operation(
            "a recording opens only in a private window of its own".into(),
        ));
    }
    context.restrict_to_reading();
    tilo_resource::recording::Recording::load(
        file.bytes(),
        &tilo_resource::recording::Limits::default(),
    )
    .map_err(|error| LaunchError::Operation(error.to_string()))
}

/// The item a launch on a record asked to show: the resource its `open`
/// names.
fn revealed(target: &LaunchTarget) -> Option<String> {
    match target {
        LaunchTarget::Record {
            reveal: Some(address),
            ..
        } if address.action == tilo_actions::remote::APPLICATION_OPEN_ACTION => {
            Some(address.resource.clone())
        }
        _ => None,
    }
}

/// Join the content store the host lent a window to its record's route:
/// content the record references and this device lacks is asked of the
/// route, and content the app stores reaches the record's other readers.
/// A capture keeps its fixture's store as it is: it reaches nothing outside.
fn document_content(context: &mut ProcessContext) -> Result<(), LaunchError> {
    if context.capture().is_some() {
        return Ok(());
    }
    let Some(local) = context.content().cloned() else {
        return Ok(());
    };
    let route = resource_route(context)?;
    let link = tilo_ui_base::collaboration::document_content::ContentLink::of_window();
    // The window keeps its content link: another window of the app on this thread
    // reads its own, never this one's queue of wants.
    tilo_ui_base::collaboration::document_content::keep_link(
        &context.wake_handle(),
        Arc::clone(&link),
    );
    // Whatever carries the record's content for the process (a browser
    // page's replication) asks for and hands over this window's too, and
    // what it stores is sealed for the members of the record's workspace.
    tilo_ui_base::collaboration::document_content::ContentLink::show_on_route(&route, &link);
    context.provide_content(
        tilo_ui_base::collaboration::document_content::DocumentContent::store(local, link),
    );
    Ok(())
}

/// The route of the record the window opens, or the refusal to open it:
/// content the app would store for a record this host cannot name a route for
/// is sealed under a key of its own, which nothing carries to the record's
/// other readers and no member of its workspace holds. A window that would
/// lose what it writes is refused before it writes, not sealed for nobody.
///
/// # Errors
/// As [`route_in`].
pub(super) fn resource_route(
    context: &ProcessContext,
) -> Result<tilo_ui_base::collaboration::RouteSpec, LaunchError> {
    route_in(context.records(), context.app_id(), &context.record().id)
}

/// The route of `record` in `records`, `app`'s store, or the refusal to open a window on it.
///
/// # Errors
/// A record whose workspace this host cannot name: a window opened outside
/// every workspace, or on a profile opened by a name rather than an id.
fn route_in(
    records: &ui_core::resource::ResourceStore,
    app: &str,
    record: &DocumentId,
) -> Result<tilo_ui_base::collaboration::RouteSpec, LaunchError> {
    records.wire_route(record).map_err(|error| {
        LaunchError::Operation(format!(
            "{app} has no workspace on this device to keep its records in ({error})"
        ))
    })
}

/// Prepare a capture's scene before its view is built: the app's fixture,
/// then the versions other people wrote, merged as their changes arrive.
fn prepare_capture<A: ResourceApp>(
    context: &mut ProcessContext,
    scenario: Option<&str>,
) -> Result<Run, LaunchError> {
    let mut fixture =
        Fixture::<A::Resource>::new(scenario, context.author(), context.content().cloned());
    A::fixture(&mut fixture);
    if context.content().is_none()
        && let Some(content) = fixture.lent_content()
    {
        context.provide_content(content.clone());
    }
    if fixture.has_history() {
        merge_peer(context, &mut |peer| {
            fixture.write_history(peer, &action_registry::<A>())
        })?;
    }
    Ok(fixture.into_run())
}

/// Merge into the record `context` opened what `write` writes into a fork
/// of it, as another member's changes arrive.
fn merge_peer(
    context: &ProcessContext,
    write: &mut dyn FnMut(&mut tilo_resource::Doc) -> Result<(), String>,
) -> Result<(), LaunchError> {
    let mut peer = context
        .read_resource(|doc| doc.clone().fork())
        .map_err(LaunchError::Record)?;
    write(&mut peer).map_err(LaunchError::Operation)?;
    context
        .merge_peer_document(peer.save())
        .map_err(LaunchError::Operation)?;
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
const CAPTURE_INSTANCE: u64 = 1;

#[cfg(not(target_arch = "wasm32"))]
/// The permissions of `author` holding `role` in `workspace`, shared with the
/// host so a later role change reaches the running app. They name the
/// workspace the app keeps its records in, so what the host decides about
/// those records is what the app is granted there.
pub(crate) fn role_permissions(
    workspace: &str,
    author: &str,
    role: Role,
) -> tilo_ui_base::permissions::SharedPermissions {
    tilo_ui_base::permissions::SharedPermissions::new(PermissionContext::member_of(
        workspace,
        &tilo_ui_base::permissions::MemberId::person(author),
        role,
    ))
}

/// Prepare `definition` on `host`: the record a launch named, else the one
/// the instance showed last, else the workspace's default.
#[cfg(tilo_native_runner)]
pub(crate) fn prepare_on(
    host: &mut ApplicationOwner,
    definition: &ApplicationDefinition,
    instance: u64,
    named: Option<DocumentId>,
    inputs: ComponentRunnerInputs,
) -> Result<PreparedComponentInputs, LaunchError> {
    // A workspace's records are read on its route too: a host that cannot name one is refused
    // here, saying so, before the record is read or anything is written.
    route_in(
        &host.resources_for(definition)?,
        definition.app_id(),
        &DocumentId::workspace_default(),
    )?;
    let kind = definition.resource_type.clone();
    match named {
        // A launch naming the workspace's own record (a notification about
        // something in it) opens it as a launch naming nothing does.
        Some(id) if !(id.is_package_primary() && definition.keeps_workspace_resource()) => host
            .prepare_resource(
                definition,
                instance,
                DocumentCheckout::new(id, kind),
                inputs,
            ),
        _ => host.prepare_default(
            definition,
            instance,
            DocumentCheckout::new(DocumentId::workspace_default(), kind),
            None,
            inputs,
        ),
    }
}

/// Prepare `A` for a launch: the record its arguments name, and the
/// arguments handed to the component for [`ResourceApp::launch`]. A first
/// argument naming a file of a type the app [opens](ResourceApp::OPENS) is
/// also handed over as that file, as a desktop handing it the file would.
#[cfg(tilo_native_runner)]
fn prepare_app<A: ResourceApp>(
    definition: &ApplicationDefinition,
    args: &LaunchArgs,
) -> Result<PreparedComponentInputs, LaunchError> {
    prepare_hooked(hooks::<A>(), definition, args)
}

#[cfg(tilo_native_runner)]
fn prepare_hooked(
    app: &AppHooks,
    definition: &ApplicationDefinition,
    args: &LaunchArgs,
) -> Result<PreparedComponentInputs, LaunchError> {
    let kept_here = kept_on_this_device(app, definition);
    // A window brought back, which its launch names nothing for, opens on the file or folder it
    // showed as the launch that first opened it there did.
    let restored = if args.positionals().is_empty()
        && !isolated(args, app.private_windows && args.has(PRIVATE_FLAG))
        && args.value("--data-dir").is_none()
        && kept_here.is_none()
    {
        NativeProcess::window_shows(definition)?
            .map(|path| vec![path.to_string_lossy().into_owned()])
    } else {
        None
    };
    let (record, arguments) =
        resource_argument(restored.as_deref().unwrap_or_else(|| args.positionals()));
    let options = launch_options(app, args);
    let named = record.or_else(|| {
        (app.launch_resource)(&LaunchInput::new(arguments, None).with_options(&options))
    });
    // A recording replays in a private window: nothing it shows reaches
    // the person's records, devices or members.
    let private = (app.private_windows && args.has(PRIVATE_FLAG)) || names_recording(arguments);
    let mut prepared = prepare_named(definition, args, named, private, kept_here.as_deref())?;
    // A window someone looks at shows before its record. What drives or photographs one acts
    // on the record from its first step: it waits for it.
    let drives = args.headless.is_some()
        || args.shots.is_some()
        || args.responsive_matrix.is_some()
        || args.native_audit;
    if drives && let Some(record) = prepared.context.resource_opening() {
        record.take_in(true).map_err(LaunchError::Record)?;
    }
    hand_launch(definition, arguments, &mut prepared)?;
    prepared.context.provide_launch_options(options);
    // The session launches the target of a playback handoff with where it
    // stood in its environment; a capture continues nothing, and neither does
    // a later window the shell opens in the process.
    if args.shots.is_none()
        && ui_core::window_link::requested_instance().is_none()
        && let Some(handoff) = process_handoff()
    {
        prepared.context.provide_handoff(handoff);
    }
    Ok(prepared)
}

/// Whether the launch names a recording to replay: a first argument that
/// is a path to one.
#[cfg(tilo_native_runner)]
fn names_recording(arguments: &[String]) -> bool {
    arguments
        .first()
        .filter(|argument| !argument.contains("://"))
        .and_then(|path| tilo_ui_base::dnd::mime_for_path(std::path::Path::new(path)))
        == Some(tilo_content::RECORDING.mime)
}

/// The playback handoff the session handed this process at its start.
#[cfg(tilo_native_runner)]
fn process_handoff() -> Option<ui_core::application::PlaybackHandoff> {
    let position_ms = std::env::var("TILO_MEDIA_HANDOFF_POSITION_MS")
        .ok()?
        .parse()
        .ok()?;
    Some(ui_core::application::PlaybackHandoff {
        position_ms,
        resume_playing: std::env::var("TILO_MEDIA_HANDOFF_RESUME_PLAYING")
            .is_ok_and(|value| value == "1"),
        title: std::env::var("TILO_MEDIA_HANDOFF_TITLE")
            .ok()
            .filter(|title| !title.trim().is_empty()),
    })
}

/// The options of `A`'s own that `args` named, in declaration order.
#[cfg(tilo_native_runner)]
fn launch_options(app: &AppHooks, args: &LaunchArgs) -> Vec<ui_core::application::LaunchOption> {
    let options = app.options.iter().filter_map(|&name| {
        args.value(name)
            .map(|value| ui_core::application::LaunchOption {
                name: name.to_owned(),
                value: Some(value.to_owned()),
            })
    });
    let flags = app
        .flags
        .iter()
        .filter(|&&name| args.has(name))
        .map(|&name| ui_core::application::LaunchOption {
            name: name.to_owned(),
            value: None,
        });
    options.chain(flags).collect()
}

/// Hand a prepared component what its launch named: the arguments, and the
/// file the first one names when the app opens its type and it is a path
/// rather than a URL.
#[cfg(tilo_native_runner)]
pub(crate) fn hand_launch(
    definition: &ApplicationDefinition,
    arguments: &[String],
    prepared: &mut PreparedComponentInputs,
) -> Result<(), LaunchError> {
    hand_launch_to(definition, arguments, &mut prepared.context)
}

/// Hand `context` what a launch naming `arguments` names: the arguments,
/// and on a device with paths the file the first one names when the app
/// opens its type.
#[cfg_attr(
    not(tilo_native_runner),
    expect(
        clippy::unnecessary_wraps,
        reason = "only a device with paths reads a file"
    )
)]
fn hand_launch_to(
    definition: &ApplicationDefinition,
    arguments: &[String],
    context: &mut ProcessContext,
) -> Result<(), LaunchError> {
    // A URL (`https://…`, `file://…`) is an argument for the app to resolve,
    // not a path on this device.
    #[cfg(tilo_native_runner)]
    if let Some(path) = arguments
        .first()
        .filter(|argument| !argument.contains("://"))
        .map(std::path::Path::new)
        && let Some(file) = launched_file(definition, path)?
    {
        context.provide_open_file(file);
    }
    #[cfg(not(tilo_native_runner))]
    let _ = definition;
    context.provide_launch_arguments(arguments.to_vec());
    Ok(())
}

/// Open, in the window `reopener` belongs to, what a launch of `app` naming
/// `arguments` opens ([`ResourceApp::launch_resource`], [`ResourceApp::launch`]):
/// the component built for that record takes the window, its own record and
/// history with it. The store's refusal of the record, an unreadable file
/// or the app's refusal to build leaves the window as it is, and is the
/// answer the returned outcome gives.
fn open_in_place(
    app: &'static AppHooks,
    reopener: &ui_core::application::Reopener,
    arguments: Vec<String>,
) -> ui_core::application::ReplacementOutcome {
    // Built when the host takes it: the record store is busy while the
    // asking view handles the event or operation that asked.
    reopener.replace_with(move |reopener| {
        let definition = (app.definition)();
        // A record named by its uri (one of the app's documents a list
        // offers) opens as a launch naming it does.
        let (named, rest) = resource_argument(&arguments);
        let record = named
            .or_else(|| (app.launch_resource)(&LaunchInput::new(rest, None)))
            .unwrap_or_else(DocumentId::minted);
        let (mut context, target) = reopener.prepare(definition, record)?;
        hand_launch_to(definition, rest, &mut context)?;
        let component = definition.instantiate(context, target, RestoreInput::None)?;
        Ok(ui_core::application::ComponentReplacement::new(
            component, arguments,
        ))
    })
}

/// The file at `path` to hand the component, when `definition` opens its
/// type; `None` for any other argument (a URL, a folder, another type).
#[cfg(tilo_native_runner)]
fn launched_file(
    definition: &ApplicationDefinition,
    path: &std::path::Path,
) -> Result<Option<ui_core::application::OpenFile>, LaunchError> {
    // A regular file whose name says nothing of its type is plain bytes.
    let Some(media_type) = tilo_ui_base::dnd::mime_for_path(path)
        .or_else(|| path.is_file().then_some("application/octet-stream"))
        .filter(|media_type| definition.opens_media_type(media_type))
    else {
        return Ok(None);
    };
    let name = path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    );
    let unreadable = |error: String| {
        LaunchError::Operation(format!("{} cannot be read: {error}", path.display()))
    };
    let bytes = tilo_files::file_at(path)
        .and_then(|(dir, file)| tilo_files::blocking::read(&dir, &file))
        .map_err(|error| unreadable(error.to_string()))?;
    let (file, _receipt) =
        ui_core::application::OpenFile::new(&name, media_type, bytes).map_err(unreadable)?;
    Ok(Some(file))
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn prepare_definition(
    definition: &ApplicationDefinition,
    args: &LaunchArgs,
) -> Result<PreparedComponentInputs, LaunchError> {
    let (record, _) = resource_argument(args.positionals());
    prepare_named(definition, args, record, false, None)
}

/// Where an app that takes part in no workspace runtime keeps its window's records when nothing
/// handed it a connection to Tilo on the device: a profile of this device's own, as a
/// `--data-dir` launch keeps them. Such an app runs only here, or is Tilo on this device itself
/// (a phone's app, which starts the device's daemon in its own process and so has no runtime to
/// attach to) and has handed its window no connection: until a person's identity has the phone.
/// `None` for every other launch, which reads its records over its connection.
#[cfg(tilo_native_runner)]
fn kept_on_this_device(
    app: &AppHooks,
    definition: &ApplicationDefinition,
) -> Option<std::path::PathBuf> {
    if app.attaches_runtime || ui_core::launch::connection_handed() || NativeProcess::connected() {
        return None;
    }
    tilo_paths::data_dir().map(|data| data.join(definition.app_id()))
}

/// The record a launch names with a first argument
/// `tilo-record:<id>` ([`DocumentId::uri`]), as the session launches an app to
/// run an action on one of its records, and the arguments after it.
fn resource_argument(arguments: &[String]) -> (Option<DocumentId>, &[String]) {
    match arguments.split_first() {
        Some((first, rest)) => match DocumentId::from_uri(first) {
            Some(record) => (Some(record), rest),
            None => (None, arguments),
        },
        None => (None, arguments),
    }
}

#[cfg(all(test, tilo_native_runner))]
mod resource_argument_tests {
    use super::*;

    #[test]
    fn a_record_uri_names_the_record_and_is_not_handed_to_the_app() {
        let record = DocumentId::minted();
        let arguments = vec![record.uri(), "--then".to_owned()];
        let (named, rest) = resource_argument(&arguments);
        assert_eq!(named, Some(record));
        assert_eq!(rest, ["--then".to_owned()]);

        let plain = vec!["notes.md".to_owned()];
        assert_eq!(resource_argument(&plain), (None, plain.as_slice()));
        assert_eq!(resource_argument(&[]), (None, [].as_slice()));
    }
}

/// Whether the launch is a capture's window: a screenshot, or a headless drive that names a
/// scenario, built from that scenario's fixtures on a profile of its own.
#[cfg(tilo_native_runner)]
fn captured(args: &LaunchArgs) -> bool {
    args.shots.is_some()
        || args.responsive_matrix.is_some()
        || (args.headless.is_some() && args.scenario.is_some())
}

/// Whether the launch runs on a private profile removed with the process: captures, audits,
/// headless driving, first-frame measurements and private windows never touch the person's, unless
/// a headless drive or a measurement names its profile with `--data-dir`.
#[cfg(tilo_native_runner)]
fn isolated(args: &LaunchArgs, private: bool) -> bool {
    private
        || captured(args)
        || args.native_audit
        || ((args.headless.is_some() || args.first_frame) && args.value("--data-dir").is_none())
}

#[cfg(tilo_native_runner)]
fn prepare_named(
    definition: &ApplicationDefinition,
    args: &LaunchArgs,
    named: Option<DocumentId>,
    private: bool,
    kept_here: Option<&std::path::Path>,
) -> Result<PreparedComponentInputs, LaunchError> {
    let inputs = ComponentRunnerInputs::from_native_process();
    let captured = captured(args);
    if isolated(args, private) {
        let profile = super::headless::Kept::fresh()?;
        let profile_root = profile.path().to_path_buf();
        let content = profile_content(profile.path());
        // The profile is a workspace of its own, which nothing else keeps: it is
        // named by an id, so the records kept in it have a route, and it holds
        // its own members' keys, so what its windows store is sealed once for
        // it and reaches nothing outside ([`super::headless::hold_workspace_keys`]).
        // Its records live as long as the process, in its memory.
        let workspace = tilo_resource::fresh_uuid().to_string();
        let mut host = ApplicationOwner::with_port(
            Some((profile.records(), true)),
            definition.app_id(),
            "capture-device".into(),
            &workspace,
            MemberId::person("capture-author"),
            role_permissions(&workspace, "capture-author", Role::Owner),
            tilo_actions::ActionContext::default(),
        )?;
        let mut prepared = prepare_on(&mut host, definition, CAPTURE_INSTANCE, named, inputs)?;
        prepared.context.provide_content(content);
        // An audit and a headless drive certify and drive the app someone
        // uses; only a screenshot, or a drive of a named scenario, is built
        // from fixtures.
        if captured {
            // A drive invokes what the scene publishes; a screenshot only
            // photographs it.
            let scenario = args.scenario.clone();
            let capture = if args.headless.is_some() {
                Capture::driven(scenario)
            } else {
                Capture::new(scenario)
            };
            prepared.context.provide_capture(capture);
        }
        if private {
            prepared.context.provide_private();
        }
        let prepared = super::headless::in_profile(prepared, &profile_root, definition.app_id());
        super::headless::hold_workspace_keys(&prepared.context, &workspace)?;
        return Ok(prepared);
    }
    if let Some(directory) = args
        .value("--data-dir")
        .map(std::path::Path::new)
        .or(kept_here)
    {
        let author = tilo_ui_base::collaboration::local_user_id();
        let mut host = super::headless::data_dir_host(
            directory,
            definition.app_id(),
            &author,
            role_permissions(
                super::headless::DATA_DIR_WORKSPACE,
                author.id(),
                Role::Owner,
            ),
        )?;
        let mut prepared = prepare_on(
            &mut host,
            definition,
            super::headless::DATA_DIR_INSTANCE,
            named,
            inputs,
        )?;
        prepared
            .context
            .provide_content(super::headless::data_dir_content(directory));
        let prepared = super::headless::in_profile(
            prepared,
            &super::headless::data_dir_root(directory),
            definition.app_id(),
        );
        super::headless::hold_workspace_keys(
            &prepared.context,
            super::headless::DATA_DIR_WORKSPACE,
        )?;
        return Ok(prepared);
    }
    let mut prepared = NativeProcess::prepare_window(definition, named, inputs)?;
    prepared
        .context
        .provide_content(tilo_content_device::for_app());
    // A launch outside every workspace keeps this device's own records, whose
    // keys no peer network holds: the window seals what it stores under
    // the workspace's own secret ([`super::headless::hold_workspace_keys`]). A window of a
    // collaboration workspace holds its members' secret instead, once its route
    // carries their keys.
    if prepared.context.records().home() == ui_core::application::PERSONAL_WORKSPACE {
        super::headless::hold_workspace_keys(
            &prepared.context,
            ui_core::application::PERSONAL_WORKSPACE,
        )?;
    }
    Ok(prepared)
}

/// The content store of a profile the launch keeps in `directory`: the
/// bytes its records name live with them, and go with them.
#[cfg(tilo_native_runner)]
pub(crate) fn profile_content(directory: &std::path::Path) -> tilo_content::ContentStore {
    tilo_content::ContentStore::in_dir(directory.join("content"))
}

/// Take the launch's actions ([`ResourceApp::launch`]) as the live host's
/// first idle tick does, before anything else drives the window.
#[cfg(tilo_native_runner)]
pub(crate) fn settle_launch(driver: &mut Driver) {
    driver.wait_until(ui_core::time::Duration::ZERO, |_| true);
}

#[cfg(not(target_arch = "wasm32"))]
fn capture<A: ResourceApp>(driver: &mut Driver, args: &LaunchArgs) -> Result<(), LaunchError> {
    settle_launch(driver);
    super::stage::stage::<A>(driver, args.scenario.as_deref())
}

#[cfg(not(target_arch = "wasm32"))]
fn shot_viewport<A: ResourceApp>(args: &LaunchArgs) -> (u32, u32) {
    A::shot_viewport(args.scenario.as_deref())
}

#[cfg(tilo_native_runner)]
struct LiveOptionsKey<A>(std::marker::PhantomData<A>);

/// The options a live launch of `A` takes: its profile, its commands and its
/// own options.
#[cfg(tilo_native_runner)]
fn live_options<A: ResourceApp>() -> &'static [&'static str] {
    leaked(TypeId::of::<LiveOptionsKey<A>>(), || {
        let mut options = vec!["--data-dir"];
        options.extend(A::commands().iter().map(|command| command.option));
        options.extend(A::OPTIONS);
        options
    })
    .as_slice()
}

#[cfg(tilo_native_runner)]
struct LiveFlagsKey<A>(std::marker::PhantomData<A>);

/// The switches a live launch of `A` takes: its own, and `--private` when it
/// opens private windows.
#[cfg(tilo_native_runner)]
fn live_flags<A: ResourceApp>() -> &'static [&'static str] {
    leaked(TypeId::of::<LiveFlagsKey<A>>(), || {
        let mut flags = A::FLAGS.to_vec();
        if A::PRIVATE_WINDOWS {
            flags.push(PRIVATE_FLAG);
        }
        flags
    })
    .as_slice()
}

/// Run the command a launch of `A` names, if it names one.
#[cfg(tilo_native_runner)]
fn run_command<A: ResourceApp>(args: &LaunchArgs) -> Result<bool, LaunchError> {
    for command in A::commands() {
        if let Some(value) = args.value(command.option) {
            (command.run)(value)
                .map_err(|error| LaunchError::Operation(format!("{}: {error}", command.option)))?;
            return Ok(true);
        }
    }
    Ok(false)
}

/// The flag that opens a private window of an app that offers them.
#[cfg(tilo_native_runner)]
const PRIVATE_FLAG: &str = "--private";

#[cfg(tilo_native_runner)]
/// The launch specification of `A`, for apps that add options to it.
pub fn launch_spec<A: ResourceApp>() -> LaunchSpec {
    let spec = LaunchSpec::from_definition(definition::<A>())
        .component_host(prepare_app::<A>)
        .native_services()
        .capture_scenario(capture::<A>)
        .shot_viewport(shot_viewport::<A>)
        .scenarios(A::scenarios())
        .reads_host(A::READS_HOST)
        .max_positionals(A::ARGUMENTS)
        .command_handler(run_command::<A>)
        .allowed_options(&["--data-dir"])
        .allowed_live_options(live_options::<A>())
        .allowed_live_flags(live_flags::<A>());
    let spec = if !A::ATTACHES_RUNTIME {
        // Such an app attaches to no runtime beside it, but a process that is its own launcher
        // (a phone running the device's daemon) attaches its window to the workspace over the
        // connection it handed it, as any launched app's window is.
        spec.runtime_attachment(|_| ui_core::launch::connection_handed())
    } else if A::PRIVATE_WINDOWS {
        // A private window is nobody's runtime member: it neither syncs
        // nor shares what it shows.
        spec.runtime_attachment(|args| !args.has(PRIVATE_FLAG))
    } else {
        spec
    };
    // An input surface types into other apps' fields: nothing drives it
    // without a person. It is a session service the compositor starts
    // without a workspace, so it has no workspace runtime to attach to.
    if A::BEHAVIOUR.window == ui_core::application::WindowRole::InputSurface {
        spec.disable_headless().runtime_attachment(|_| false)
    } else {
        spec
    }
}

#[cfg(not(tilo_native_runner))]
/// Named for the same apps that build a native [`LaunchSpec`]; the browser
/// host mounts [`definition`] itself and never runs this path.
pub fn launch_spec<A: ResourceApp>() -> crate::launch::LaunchSpec {
    crate::launch::LaunchSpec::from_definition(definition::<A>())
}

#[cfg(feature = "native")]
/// The launch specification of any application definition: the standard
/// component host, record and launch modes, without a resource type's
/// capture scenarios.
#[cfg(not(target_arch = "wasm32"))]
pub fn definition_launch_spec(definition: &'static ApplicationDefinition) -> LaunchSpec {
    LaunchSpec::from_definition(definition)
        .component_host(prepare_definition)
        .native_services()
        .allowed_options(&["--data-dir"])
        .allowed_live_options(&["--data-dir"])
}

/// Run `A`: the live window, or `--shots`, `--headless` and the other
/// standard launch modes. Needs the native runner (the default `native`
/// feature); a host without it drives [`launch_spec`] itself.
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub fn run<A: ResourceApp>() {
    /// Print `report` and end the process: what `--surface` does. An item
    /// of its own, not generic over the app, so the JSON printer is
    /// compiled once rather than in every app's `run`.
    #[inline(never)]
    fn print_surface_report(report: &serde_json::Value) -> ! {
        let code = match serde_json::to_string_pretty(report) {
            Ok(report) => {
                println!("{report}");
                0
            }
            Err(error) => {
                eprintln!("Error: {error}");
                1
            }
        };
        #[allow(
            clippy::exit,
            reason = "the surface report is the whole run of the binary"
        )]
        std::process::exit(code);
    }

    let args: Vec<String> = std::env::args().collect();
    if args.iter().skip(1).any(|arg| arg == SURFACE_FLAG) {
        print_surface_report(&super::headless::surface_report::<A>());
    }
    // A record search reads, with no window of this app: its document in,
    // what search finds in it out.
    if let Some(code) = super::resource_index::serve::<A>() {
        #[allow(
            clippy::exit,
            reason = "the record reader is the whole run of the binary"
        )]
        std::process::exit(code);
    }
    if let Some(code) = run_helper::<A>(&args) {
        #[allow(
            clippy::exit,
            reason = "a helper the binary is ends the process with its own exit code"
        )]
        std::process::exit(code);
    }
    crate::launch::run_or_exit(&launch_spec::<A>());
}

/// The flag that makes an app's binary print its
/// [`surface_report`](super::headless::surface_report)
/// instead of running: what `tilolang surface` and `tilolang check` read.
pub const SURFACE_FLAG: &str = "--surface";

/// Run the first of `A`'s [helpers](ResourceApp::helpers) that `args` is
/// for, answering the exit code the process ends with; `None` when the
/// command line is the app's own.
pub fn run_helper<A: ResourceApp>(args: &[String]) -> Option<i32> {
    for helper in A::helpers() {
        match (helper.run)(args) {
            Ok(None) => {}
            Ok(Some(code)) => return Some(code),
            Err(error) => {
                eprintln!("Error: {error}");
                return Some(1);
            }
        }
    }
    None
}
