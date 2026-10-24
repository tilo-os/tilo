//! Declaring and running a framework app on the toolkit's component runtime.

use std::any::TypeId;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Mutex, OnceLock};

use tilo_resource::{Invoke, Reader, Resource};
use ui_core::application::{
    AppComponent, ApplicationDefinition, ComponentBehaviour, ComponentFactory, HostContext,
    LaunchTarget, RestoreInput,
};
use ui_core::history_mode::checked_files::FileHistorySource;
use ui_core::i18n::EmbeddedCatalog;
use ui_core::launch::LaunchError;
use ui_core::permissions::{ActionKind, LivePermissions, PermissionScope};
use ui_core::record::{RecordId, RecordKind};
use ui_core::storage::AppStorage;
use ui_core::widget::{Element, IconDescriptor};

// Launching (live windows, captures, headless drives, test hosts) discovers
// native presentation and profiles; a browser host prepares components itself.
#[cfg(not(target_arch = "wasm32"))]
use {
    std::cell::RefCell,
    ui_core::application::{
        ApplicationHost, Capture, ComponentRunnerInputs, NativeProcessHost, PreparedComponentInputs,
    },
    ui_core::headless::FrameHarness,
    ui_core::launch::{LaunchArgs, LaunchSpec},
    ui_core::permissions::{PermissionContext, Role, UserId},
    ui_core::record::RecordCheckout,
};

use super::dispatch::Dispatcher;
use super::effect::Effects;
use super::launch::{Command, Helper, LaunchInput};
use super::live::Live;
use super::root::ResourceRoot;
use super::scenario::{Fixture, Launched, Run};
use super::view::ViewCx;
use tilo_lock::MutexRecover as _;

/// A framework app: one resource type and a view of it.
///
/// Everything else — storage, sync, collaboration, history, undo,
/// permissions, the action palette, the agent and script door, search — is
/// provided. See the [module docs](super).
pub trait ResourceApp: 'static {
    /// The resource this app edits.
    type Resource: Resource;
    /// The application id, e.g. `"so.tilo.scratchpad"`.
    const ID: &'static str;
    /// The window and launcher title.
    const TITLE: &'static str;
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
    /// [`Self::launch_record`] and [`Self::launch`] receive them.
    const ARGUMENTS: usize = 0;
    /// Whether the resource keeps document state whose versions History
    /// shows. An app whose state lives outside its document (a measurement,
    /// a device) sets `false`: it publishes no History actions and its
    /// window offers no History.
    const DOCUMENT_HISTORY: bool = true;
    /// The app brings what its windows kept before it kept one record per
    /// workspace into that record in its own seed
    /// ([`SeedCx::window_records`]), as Notes copies its pages by id. Every
    /// app keeps one record per workspace, which every window of it there
    /// opens and the first one creates; without this the framework copies
    /// the earlier windows' records into it value by value
    /// (`adopt_windows`).
    const ROOM_RECORD: bool = false;
    /// Each window shows a resource of its own: a document an editor opened
    /// (Word Processor, Code Editor, Sheets), a terminal session. A window
    /// that names none starts a new one, and two windows show two of them
    /// (or one document together, when both open it); no window mirrors
    /// another. The documents earlier windows started stay in the workspace:
    /// the app lists them with [`RecordEnv::window_documents`](super::RecordEnv::window_documents)
    /// and opens one in a window with [`RecordEnv::open_here`](super::RecordEnv::open_here).
    /// Without it the app's record is its library and settings,
    /// one per workspace, which every window of the app there shows.
    const WINDOW_RESOURCE: bool = false;
    /// The app's own command-line options that take a value
    /// (`--purpose number` or `--purpose=number`): how this window runs
    /// rather than what it opens. [`Self::launch`] reads them from its
    /// [`LaunchInput::option`], views and surfaces from
    /// [`RecordEnv::launch_option`](super::RecordEnv::launch_option).
    const OPTIONS: &'static [&'static str] = &[];
    /// The app's own switches, which take no value (`--debug-hit-areas`):
    /// [`LaunchInput::flag`], [`RecordEnv::launch_flag`](super::RecordEnv::launch_flag).
    const FLAGS: &'static [&'static str] = &[];
    /// Whether the app opens private windows (`--private`): a window on a
    /// profile of its own that is thrown away with the process — its record,
    /// settings and files never reach the person's — and that attaches to
    /// no runtime, so nothing it shows syncs or is shared. It holds none of
    /// the person's own records ([`RecordEnv::person`](super::RecordEnv::person) answers `None`), and its
    /// view tells with [`RecordEnv::is_private`](super::RecordEnv::is_private).
    const PRIVATE_WINDOWS: bool = false;
    /// The history of a file of the mounted filesystem the app shows (the
    /// file selected in a file manager), a History scope beside the
    /// resource's own: its versions are read, previewed and restored through
    /// the filesystem's projection, not the app's document. The app's
    /// surface names the file with [`SurfaceCx::file_history`](super::SurfaceCx::file_history).
    const FILE_HISTORY: Option<FileHistory> = None;
    /// What of the machine it runs on the app shows (its journal, its
    /// processes, its drives, the person's real files). A headless drive
    /// that captures it must then name one of its [scenarios](Self::scenarios)
    /// and is built from that scenario's fixtures, so no picture of the app
    /// shows the machine of whoever took it.
    const READS_HOST: &'static [ui_core::launch::HostSource] = &[];

    /// Import a former document format using host-local custody, before typed
    /// projection or resource publication. Unlike schema evolution this may
    /// depend on device services. A pending or failed import must leave the
    /// document unchanged; returning an error withholds the resource. Reopening
    /// retries the import, so retain acknowledged writes across pending retries.
    ///
    /// # Errors
    /// Returns a safe, user-visible reason why the record cannot yet open.
    fn import_record(context: &HostContext) -> Result<(), String> {
        let _ = context;
        Ok(())
    }

    /// Build the view. Called once for the live app, and again for every
    /// historical version History mode shows (with a read-only context), so
    /// it must build widgets and nothing else.
    fn view(cx: &mut ViewCx<Self::Resource>) -> Element;

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
    /// ([`RecordId::identify_file`]), a record derived from it
    /// ([`RecordId::derived`]), a minted one. `None`, the default, opens the
    /// record the app would open anyway: the one the window showed last, else
    /// the room's default. Asked by a launcher that picks the record (the
    /// command line, a test host), before the record is opened, so it must
    /// not depend on anything but `input`.
    fn launch_record(input: &LaunchInput<'_>) -> Option<RecordId> {
        let _ = input;
        None
    }

    /// The actions a launch naming `input` takes once its record is open;
    /// `created` when this launch created the record, so actions that fill
    /// a new record (the source of a new project) are taken only then. They
    /// go through the same door as every control: checked against the
    /// participant's permissions, attributed, undoable and in history. A
    /// launch that shows something rather than changing the resource (a
    /// link to one item, a file to look at before acting on it) invokes one
    /// of the app's effects ([`Invoke::effect`]) through the same door. A
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

    /// Whether the app takes part in the workspace runtime of the room it is
    /// launched in. An app that only runs on this device (an imager on a
    /// platform without the runtime) says no.
    const ATTACHES_RUNTIME: bool = true;

    /// Whether the app opens while its workspace has no identity selected,
    /// joined to nothing, instead of a window asking who works there. Only
    /// for an app whose own purpose is not the workspace's (Settings, for the
    /// device): it reads [`ui_core::workspace_identity::missing`] and shows
    /// [`ui_core::workspace_identity::required_state`] where it needs the
    /// workspace. Once an identity is chosen it opens again, joined to it.
    const OPENS_WITHOUT_IDENTITY: bool = false;

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
    /// and collaborators. Runs only for a capture, whose views are built in
    /// [`RunMode::Capture`](super::RunMode). See [`Fixture`](super::Fixture).
    fn fixture(fixture: &mut super::Fixture<Self::Resource>) {
        let _ = fixture;
    }

    /// The actions that stage the resource for a capture: `Some(name)` for
    /// `--shots --scenario NAME`, `None` for a plain `--shots`. They apply
    /// through the same door as every control; then the views stage their
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
        input: &mut ui_core::headless::FrameHarness,
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
    /// seed back. A collaborator who creates the same record concurrently
    /// seeds it too, so a seed sets values rather than adding rows.
    fn seed(cx: &SeedCx<'_>) -> Vec<Invoke<Self::Resource>> {
        let _ = cx;
        Vec::new()
    }

    /// The records the app keeps besides the one it shows: the person's own
    /// records and the settings sections that are resource models. Each is
    /// opened with the window and published beside the app's resource, so
    /// the palette and agents change it whether or not a view has shown it,
    /// and `tilo-app surface` commits its surface with the app's.
    ///
    /// ```ignore
    /// fn records(records: &mut Records<'_>) {
    ///     records.person::<WatchHistory>().settings::<Playback>();
    /// }
    /// ```
    fn records(records: &mut super::Records<'_>) {
        let _ = records;
    }
}

/// The History scope of [`ResourceApp::FILE_HISTORY`]: the files of the
/// mounted filesystem an app shows.
#[derive(Clone, Copy)]
pub struct FileHistory {
    /// What the scope is called in History: "Selected file".
    pub title: &'static str,
    /// Draws one version of a file, from its bytes as the projection read
    /// them.
    pub preview: ui_core::history_mode::checked_files::FilePreviewFactory,
}

/// What [`ResourceApp::seed`] may read: who is creating the record, and the
/// app's host — its settings, files and secrets on this device — when the
/// record is created in a window. A participant without one (an agent, a
/// room service: [`seed_record`](super::seed_record)) creates it with no
/// host: what a seed imports from the device is the device's own to seed.
pub struct SeedCx<'a> {
    pub(crate) me: &'a str,
    pub(crate) device: &'a str,
    pub(crate) host: Option<&'a HostContext>,
    pub(crate) room: Option<&'a str>,
}

impl SeedCx<'_> {
    /// Who is creating the record: the id their views read as
    /// [`RecordEnv::me`](super::RecordEnv::me) and the seed's actions read as their author.
    pub fn me(&self) -> &str {
        self.me
    }

    /// The device creating the record.
    pub fn device(&self) -> &str {
        self.device
    }

    /// The host-selected room of a new record, and its display name. An
    /// ordinary restore has no creation scope. A windowless room participant
    /// carries its explicit route without supplying device services or secrets.
    pub fn creation_workspace(&self) -> Option<(&str, &str)> {
        self.host
            .and_then(HostContext::creation_workspace)
            .map(|workspace| (workspace.id.as_str(), workspace.name.as_str()))
            .or_else(|| self.room.map(|room| (room, room)))
    }

    /// The app's host on this device; `None` for a participant without a
    /// window.
    pub fn host(&self) -> Option<&HostContext> {
        self.host
    }

    /// What the app's windows in this room kept, one record each, from
    /// before it [kept one record per room](ResourceApp::ROOM_RECORD): every
    /// window default record of its kind on this device, read-only and in
    /// id order. The room's record is created once, so a seed that copies
    /// these into it migrates them exactly once; the records themselves are
    /// left as they were. Empty for any other record, and without a host.
    pub fn window_records(&self) -> Vec<(RecordId, tilo_resource::Doc)> {
        self.host
            .map(HostContext::window_records)
            .unwrap_or_default()
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
        ui_core::app_surface::register_kind(<A::Resource as Resource>::KIND, A::ID);
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
    room_record: bool,
    create: ComponentFactory,
) -> ApplicationDefinition {
    ApplicationDefinition {
        app_id: app.id,
        title: app.title,
        icon: icon(app),
        record_kind: RecordKind::new(app.kind),
        required: vec![],
        optional: vec![],
        opens,
        room_record,
        create,
    }
}

fn icon(app: &AppHooks) -> IconDescriptor {
    IconDescriptor::Named(
        app.icon
            .unwrap_or_else(|| app.id.rsplit('.').next().unwrap_or(app.id))
            .to_owned(),
    )
}

/// What the launch path, the window and History read of an app: its
/// constants, and shims over the parts that need its types. One table per
/// app; everything that reads it is compiled once for every app.
pub(crate) struct AppHooks {
    pub(crate) id: &'static str,
    pub(crate) title: &'static str,
    icon: Option<&'static str>,
    /// [`Resource::KIND`] of the app's resource.
    pub(crate) kind: &'static str,
    /// [`Resource::LABEL`] of the app's resource.
    label: &'static str,
    behaviour: ComponentBehaviour,
    document_history: bool,
    file_history: Option<FileHistory>,
    #[cfg(tilo_native_runner)]
    private_windows: bool,
    #[cfg(tilo_native_runner)]
    options: &'static [&'static str],
    #[cfg(tilo_native_runner)]
    flags: &'static [&'static str],
    catalog: fn() -> Option<&'static EmbeddedCatalog>,
    definition: fn() -> &'static ApplicationDefinition,
    launch_record: fn(&LaunchInput<'_>) -> Option<RecordId>,
    local_operations: fn() -> Vec<tilo_actions::ActionSpec>,
    declarations: fn() -> Rc<[tilo_resource::Declaration]>,
    /// Initialize a record the launch created: the resource's own
    /// initialization, the launch's genesis and the creator's seed.
    new_record: NewRecord,
    prepare_capture: fn(&mut HostContext, Option<&str>) -> Result<Run, LaunchError>,
    /// Read the record `context` opened, build the root over it and take
    /// the launch's actions on it; or why the record does not read.
    open: OpenRoot,
    pub(crate) action_registry: fn() -> tilo_actions::ActionRegistry,
    history: fn() -> &'static ui_core::history_mode::HistoryDefinition,
    file_history_witness: fn() -> &'static ui_core::history_mode::HistoryDefinition,
}

/// [`AppHooks::new_record`]: the context, and what the launch named.
type NewRecord = fn(
    &mut HostContext,
    &[String],
    Option<&ui_core::application::OpenFile>,
    &[ui_core::application::LaunchOption],
) -> Result<(), LaunchError>;

/// [`AppHooks::open`]: the context, who edits, whether it is a capture and
/// whether the launch created the record.
type OpenRoot = fn(
    &mut HostContext,
    Editing,
    bool,
    bool,
) -> Result<Result<Box<dyn ui_core::widget::AppWidget>, String>, LaunchError>;

struct Hooks<A>(std::marker::PhantomData<A>);

impl<A: ResourceApp> Hooks<A> {
    const HOOKS: AppHooks = AppHooks {
        id: A::ID,
        title: A::TITLE,
        icon: A::ICON,
        kind: <A::Resource as Resource>::KIND,
        label: <A::Resource as Resource>::LABEL,
        behaviour: A::BEHAVIOUR,
        document_history: A::DOCUMENT_HISTORY,
        file_history: A::FILE_HISTORY,
        #[cfg(tilo_native_runner)]
        private_windows: A::PRIVATE_WINDOWS,
        #[cfg(tilo_native_runner)]
        options: A::OPTIONS,
        #[cfg(tilo_native_runner)]
        flags: A::FLAGS,
        catalog: A::catalog,
        definition: definition::<A>,
        launch_record: A::launch_record,
        local_operations: A::local_operations,
        declarations: declarations_of::<A::Resource>,
        new_record: new_record::<A>,
        prepare_capture: prepare_capture::<A>,
        open: open_root::<A>,
        action_registry: action_registry::<A>,
        history: super::preview::history_definition::<A>,
        file_history_witness: super::preview::file_history_witness::<A>,
    };
}

/// `A`'s [`AppHooks`].
pub(crate) fn hooks<A: ResourceApp>() -> &'static AppHooks {
    &Hooks::<A>::HOOKS
}

fn declarations_of<R: Resource>() -> Rc<[tilo_resource::Declaration]> {
    tilo_resource::vocabulary::<R>().declarations()
}

/// Who may edit the resource a root shows, and what it is built for.
pub(crate) enum Editing {
    /// The participant (`me`) and their permissions, followed while the app
    /// runs, in a window someone uses or a capture.
    Live {
        permissions: LivePermissions,
        me: String,
        run: Run,
    },
    /// Never: a past version in History mode.
    ReadOnly,
}

/// What a root is built with besides its resource type: who edits it and
/// how, what it was built for, and where its settings are kept.
struct RootShell {
    /// The permission that makes the app's resource editable.
    scope: PermissionScope,
    permissions: Option<LivePermissions>,
    read_only: bool,
    me: Option<String>,
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
        let scope = PermissionScope::app(app.id, ActionKind::Undoable);
        let (permissions, read_only, me, run) = match editing {
            Editing::Live {
                permissions,
                me,
                run,
            } => {
                let read_only = !permissions.check(&scope);
                (Some(permissions), read_only, Some(me), run)
            }
            Editing::ReadOnly => (None, true, None, Run::past()),
        };
        let run = run.for_participant(me.as_deref());
        let storage = super::settings::ViewStorage::new(storage, hosted.attach(app.id, app.title));
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

/// Build the framework root for a resource state: the live handle, the
/// dispatcher, the published resource and the app's view.
pub(crate) fn build_root<A: ResourceApp>(
    reader: Reader<A::Resource>,
    editing: Editing,
    storage: AppStorage,
    hosted: super::settings::Hosted,
) -> ResourceRoot {
    let app = hooks::<A>();
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
    let label = A::label(reader.state());
    let published = tilo_resource::publish::<A::Resource>(app.kind, &label, reader.state());
    let live = Live::new(reader);
    let alive = Rc::new(());
    let dispatch = Dispatcher::new(app.id, app.kind, read_only, me.as_deref());
    let dispatch = if permissions.is_none() {
        dispatch.past()
    } else {
        dispatch
    };
    // A window someone uses opens the records the app keeps beside its own,
    // so they are published before any view shows them.
    if permissions.is_some() {
        A::records(&mut super::Records::new(&storage).with_base(live.clone(), dispatch.clone()));
    }
    let effects = if runs_effects {
        Some(Effects::live(dispatch.clone(), live.clone()))
    } else {
        runs_invoked_effects.then(|| Effects::invoked(dispatch.clone(), live.clone()))
    };
    let awaiting = run.room.clone();
    let mut cx = ViewCx::root(live, &alive, dispatch, published, run, storage);
    if let Some(effects) = effects {
        cx.env.effects = effects;
    }
    // A record the room may hold shows once the room's holdings arrived;
    // the app's view says nothing about it. Holdings already here (no
    // shell hosts the window, or the room synced) need no stand-in.
    let content: Element = match awaiting {
        Some(holdings) if holdings.with(|room| room.ready().is_none()) => Box::new(
            cx.arriving(&holdings, |cx, ()| A::view(cx))
                // The window's whole view: a page.
                .shaped(ui_core::widget::SkeletonShape::Page),
        ),
        _ => A::view(&mut cx),
    };
    let live = permissions.is_some();
    let root = ResourceRoot::new(app.title, icon(app), label, cx, alive, content)
        .following(permissions, scope)
        .with_presentation((app.local_operations)());
    if !app.document_history {
        return root.without_history();
    }
    if live {
        #[cfg(not(target_family = "wasm"))]
        return root.drawing_past(super::preview::draw_past::<A>);
    }
    root
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
    author: &str,
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
    author: &str,
    actions: &[&dyn tilo_actions::Action],
    encoded: Vec<tilo_actions::EncodedAction>,
) -> Result<(), String> {
    let writer = ui_core::history_source::authors::device_actor(author, doc.get_actor());
    for (action, encoded) in actions.iter().zip(encoded) {
        let context = tilo_actions::ActionContext::for_resource(app.kind, app.kind, app.label)
            .with_app(app.id, app.title)
            .with_invoker(tilo_actions::ActionInvoker::actor(author))
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
fn new_record<A: ResourceApp>(
    context: &mut HostContext,
    arguments: &[String],
    file: Option<&ui_core::application::OpenFile>,
    options: &[ui_core::application::LaunchOption],
) -> Result<(), LaunchError> {
    let genesis = A::genesis(
        &LaunchInput::new(arguments, file)
            .with_options(options)
            .with_content(context.content()),
    );
    let author = context.author().0.clone();
    let seed = A::seed(&SeedCx {
        me: &author,
        device: context.device_id(),
        host: Some(context),
        room: None,
    });
    // What the app's windows kept, one record each, before it kept one per
    // workspace; an app that adopts them in its own seed
    // (`ResourceApp::ROOM_RECORD`) has done so.
    let windows = if A::ROOM_RECORD {
        Vec::new()
    } else {
        context.window_records()
    };
    initialize_record(
        context,
        &mut |doc| {
            <A::Resource as Resource>::initialize(doc).map_err(|error| error.to_string())?;
            write_genesis(doc, &genesis)
        },
        &mut |doc| {
            apply_seed::<A>(doc, &author, &seed)?;
            super::adopt_windows::adopt(doc, &windows, &|window| {
                tilo_resource::evolution::migrate::<A::Resource>(window)
                    .map(drop)
                    .map_err(|error| error.to_string())
            })
        },
    )
}

/// What initializing a new record writes into it.
type RecordWrite<'a> = &'a mut dyn FnMut(&mut tilo_resource::Doc) -> Result<(), String>;

/// Initialize the new record `context` opened: `genesis` into its genesis,
/// then `seed`. Not generic over the app, so the record store's
/// initialization is compiled once.
fn initialize_record(
    context: &mut HostContext,
    genesis: RecordWrite<'_>,
    seed: RecordWrite<'_>,
) -> Result<(), LaunchError> {
    context
        .initialize_new_record_seeded(|doc| genesis(doc), |doc| seed(doc))
        .map_err(LaunchError::Record)
}

/// Bring an older record up to the resource's storage version; what it
/// answers when it cannot. Only an older document is written to: opening
/// another view of a current record must not commit what one already open
/// has pending.
fn migrate_older(
    context: &HostContext,
    version: u32,
    migrate: &mut dyn FnMut(&mut tilo_resource::Doc) -> Result<bool, String>,
) -> Result<Result<(), String>, LaunchError> {
    let stored = context
        .read_record(tilo_resource::evolution::stored_version)
        .map_err(LaunchError::Record)?;
    Ok(if stored < version {
        context.migrate_record(|doc| migrate(doc)).map(drop)
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
    context: &mut HostContext,
    editing: Editing,
    capture: bool,
    created: bool,
) -> Result<Result<Box<dyn ui_core::widget::AppWidget>, String>, LaunchError> {
    if let Err(reason) = A::import_record(context) {
        return Ok(Err(reason));
    }
    let migrated = migrate_older(context, <A::Resource as Resource>::VERSION, &mut |doc| {
        tilo_resource::evolution::migrate::<A::Resource>(doc).map_err(|error| error.to_string())
    })?;
    let read = context
        .read_record(|doc| {
            Reader::<A::Resource>::read(doc)
                .map_err(|error| tilo_resource::Unreadable::of::<A::Resource>(doc, &error).reason)
        })
        .map_err(LaunchError::Record)?;
    let reader = match migrated.and(read) {
        Ok(reader) => reader,
        Err(reason) => return Ok(Err(reason)),
    };
    let root = build_root::<A>(reader, editing, context.storage(), hosted(context));
    if let Some(typed) = root.typed::<A::Resource>() {
        let record = typed.live.get();
        let launch = root.launch().with_record(&*record);
        if !capture || !launch.is_empty() {
            for invoke in A::launch(&launch, created) {
                typed.dispatch.submit(invoke);
            }
        }
    }
    Ok(Ok(root.into_widget()))
}

/// What a window's root keeps of the person and the device.
fn hosted(context: &HostContext) -> super::settings::Hosted {
    super::settings::Hosted {
        // A private window keeps nothing of the person's: their own
        // records are not even opened.
        person: (!context.is_private())
            .then(|| context.person_records().cloned())
            .flatten(),
        device: Some(context.device_id().to_owned()),
        room: Some((
            context.records().clone(),
            context.author().0.clone(),
            context.live_permissions(),
        )),
    }
}

/// The window of a record that does not read as the app's resource.
fn unopened(app: &AppHooks, context: HostContext, reason: &str) -> AppComponent {
    let unopened = super::root::Unopened::new(
        app.title,
        icon(app),
        (app.kind, app.label),
        &(app.declarations)(),
        reason,
    );
    let mut component = AppComponent::new(context, unopened.boxed(), (app.action_registry)());
    component.set_behaviour(app.behaviour);
    component
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "the toolkit's ComponentFactory signature passes the target owned"
)]
fn create<A: ResourceApp>(
    context: HostContext,
    target: LaunchTarget,
    _restore: RestoreInput,
) -> Result<AppComponent, LaunchError> {
    create_component(hooks::<A>(), context, &target)
}

fn create_component(
    app: &'static AppHooks,
    mut context: HostContext,
    target: &LaunchTarget,
) -> Result<AppComponent, LaunchError> {
    if let Some(catalog) = (app.catalog)() {
        catalog.ensure();
    }
    // A record of its own, or the room's record an app keeps one of
    // (`ResourceApp::ROOM_RECORD`), opened for the first time.
    let created = matches!(target, LaunchTarget::New | LaunchTarget::PrimaryNew)
        || context.creation_workspace().is_some();
    record_content(&mut context);
    let file = context.take_open_file();
    let arguments = context.take_launch_arguments();
    let options = context.take_launch_options();
    if created {
        (app.new_record)(&mut context, &arguments, file.as_ref(), &options)?;
    }
    let capture = context.capture().is_some();
    let file_history = app.file_history.map(|_| FileHistorySource::default());
    // A window someone uses can open another record in its place; a
    // capture shows the one it staged.
    let opener = (!capture).then(|| {
        let reopener = context.reopener();
        super::launch::OpenHere::new(move |arguments| open_in_place(app, &reopener, arguments))
    });
    let run = match context.capture().cloned() {
        Some(capture) => (app.prepare_capture)(&mut context, capture.scenario())?
            .driven_by_drive(capture.is_driven()),
        // The room's record, not on this device when the window opened:
        // the room may hold it, so the view waits for what the room holds.
        None => Run::live(context.content().cloned())
            .in_private(context.is_private())
            .awaiting_room(
                matches!(target, LaunchTarget::PrimaryNew).then(ui_core::room_sync::holdings),
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
        })
        .opening_with(opener)
        .showing_file_history(
            file_history
                .clone()
                .map(|source| (source, (app.file_history_witness)())),
        );
    let permissions = context.live_permissions();
    let me = context.author().0.clone();
    let editing = Editing::Live {
        permissions,
        me,
        run,
    };
    let root = match (app.open)(&mut context, editing, capture, created)? {
        Ok(root) => root,
        Err(reason) => return Ok(unopened(app, context, &reason)),
    };
    let mut component = AppComponent::new(context, root, (app.action_registry)());
    component.set_behaviour(app.behaviour);
    if app.document_history {
        let history = (app.history)();
        match file_history {
            Some(files) => component.register_document_and_file_history(history, files),
            None => component.register_document_history(history),
        }
        .map_err(|error| LaunchError::Operation(error.to_string()))?;
    }
    Ok(component)
}

/// Join the content store the host lent a window to its record's route:
/// content the record references and this device lacks is asked of the
/// route, and content the app stores reaches the record's other readers.
/// A capture keeps its fixture's store as it is: it reaches nothing outside.
fn record_content(context: &mut HostContext) {
    if context.capture().is_some() {
        return;
    }
    let Some(local) = context.content().cloned() else {
        return;
    };
    let lane = ui_core::collaboration::record_content::ContentLane::of_record(
        context.app_id(),
        &context.record().id,
    );
    context
        .provide_content(ui_core::collaboration::record_content::RecordContent::store(local, lane));
}

/// Prepare a capture's scene before its view is built: the app's fixture,
/// then the versions other people wrote, merged as their changes arrive.
fn prepare_capture<A: ResourceApp>(
    context: &mut HostContext,
    scenario: Option<&str>,
) -> Result<Run, LaunchError> {
    let mut fixture = Fixture::<A::Resource>::new(scenario, context.content().cloned());
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
/// of it, as another participant's changes arrive.
fn merge_peer(
    context: &HostContext,
    write: &mut dyn FnMut(&mut tilo_resource::Doc) -> Result<(), String>,
) -> Result<(), LaunchError> {
    let mut peer = context
        .read_record(|doc| doc.clone().fork())
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
pub(crate) const DATA_DIR_INSTANCE: u64 = 0;

#[cfg(not(target_arch = "wasm32"))]
/// The host of an explicit `--data-dir` profile: one personal workspace in
/// that directory. Tools that seed or inspect such a profile open it the same
/// way the app does.
pub(crate) fn data_dir_host(
    directory: &std::path::Path,
    author: &UserId,
    permissions: Rc<RefCell<PermissionContext>>,
) -> Result<ApplicationHost, LaunchError> {
    let root = std::path::absolute(directory).map_err(super::headless::operation)?;
    ApplicationHost::new(
        root,
        "local-device".into(),
        "personal",
        author.clone(),
        permissions,
        tilo_actions::ActionContext::default(),
    )
}

#[cfg(not(target_arch = "wasm32"))]
/// The permissions of `author` holding `role`, shared with the host so a
/// later role change reaches the running app.
pub(crate) fn role_permissions(author: &str, role: Role) -> Rc<RefCell<PermissionContext>> {
    Rc::new(RefCell::new(PermissionContext::member(author, role)))
}

/// Prepare `definition` on `host`: the record a launch named, else the one
/// the instance showed last, else the room's default.
#[cfg(tilo_native_runner)]
pub(crate) fn prepare_on(
    host: &mut ApplicationHost,
    definition: &ApplicationDefinition,
    instance: u64,
    named: Option<RecordId>,
    inputs: ComponentRunnerInputs,
) -> Result<PreparedComponentInputs, LaunchError> {
    let kind = definition.record_kind.clone();
    match named {
        // A launch naming the workspace's own record (a notification about
        // something in it) opens it as a launch naming nothing does.
        Some(id) if !(id.is_package_primary() && definition.keeps_room_record()) => {
            host.prepare_record(definition, instance, RecordCheckout::new(id, kind), inputs)
        }
        _ => host.prepare_default(
            definition,
            instance,
            RecordCheckout::new(RecordId::room_default(), kind),
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
    let (record, arguments) = record_argument(args.positionals());
    let options = launch_options(app, args);
    let named = record
        .or_else(|| (app.launch_record)(&LaunchInput::new(arguments, None).with_options(&options)));
    let private = app.private_windows && args.has(PRIVATE_FLAG);
    let mut prepared = prepare_named(definition, args, named, private)?;
    hand_launch(definition, arguments, &mut prepared)?;
    prepared.context.provide_launch_options(options);
    // The session launches the target of a playback handoff with where it
    // stood in its environment; a capture continues nothing, and neither does
    // a later window the shell opens in the process.
    if args.shots.is_none()
        && ui_core::window_door::requested_instance().is_none()
        && let Some(handoff) = process_handoff()
    {
        prepared.context.provide_handoff(handoff);
    }
    Ok(prepared)
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
///
/// # Errors
/// A file of a type the app opens that cannot be read.
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
///
/// # Errors
/// A file of a type the app opens that cannot be read.
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
    context: &mut HostContext,
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
/// `arguments` opens ([`ResourceApp::launch_record`], [`ResourceApp::launch`]):
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
        let (named, rest) = record_argument(&arguments);
        let record = named
            .or_else(|| (app.launch_record)(&LaunchInput::new(rest, None)))
            .unwrap_or_else(RecordId::minted);
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
///
/// # Errors
/// A file of a type the app opens that cannot be read.
#[cfg(tilo_native_runner)]
fn launched_file(
    definition: &ApplicationDefinition,
    path: &std::path::Path,
) -> Result<Option<ui_core::application::OpenFile>, LaunchError> {
    // A regular file whose name says nothing of its type is plain bytes.
    let Some(media_type) = ui_core::dnd::mime_for_path(path)
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
    let (record, _) = record_argument(args.positionals());
    prepare_named(definition, args, record, false)
}

/// The record a launch names with a first argument
/// `tilo-record:<id>` ([`RecordId::uri`]), as the session launches an app to
/// run an action on one of its records, and the arguments after it.
fn record_argument(arguments: &[String]) -> (Option<RecordId>, &[String]) {
    match arguments.split_first() {
        Some((first, rest)) => match RecordId::from_uri(first) {
            Some(record) => (Some(record), rest),
            None => (None, arguments),
        },
        None => (None, arguments),
    }
}

#[cfg(all(test, tilo_native_runner))]
mod record_argument_tests {
    use super::*;

    #[test]
    fn a_record_uri_names_the_record_and_is_not_handed_to_the_app() {
        let record = RecordId::minted();
        let arguments = vec![record.uri(), "--then".to_owned()];
        let (named, rest) = record_argument(&arguments);
        assert_eq!(named, Some(record));
        assert_eq!(rest, ["--then".to_owned()]);

        let plain = vec!["notes.md".to_owned()];
        assert_eq!(record_argument(&plain), (None, plain.as_slice()));
        assert_eq!(record_argument(&[]), (None, [].as_slice()));
    }
}

#[cfg(tilo_native_runner)]
fn prepare_named(
    definition: &ApplicationDefinition,
    args: &LaunchArgs,
    named: Option<RecordId>,
    private: bool,
) -> Result<PreparedComponentInputs, LaunchError> {
    let inputs = ComponentRunnerInputs::from_native_process();
    // Captures, audits, headless driving, first-frame measurements and
    // private windows never touch the user's profile: they run on a private
    // one removed with the process, unless a headless drive or a
    // measurement names its profile with `--data-dir`.
    // A headless drive that names a scenario is a capture's window: built
    // from that scenario's fixtures, on a profile of its own.
    let captured = args.shots.is_some()
        || args.responsive_matrix.is_some()
        || (args.headless.is_some() && args.scenario.is_some());
    let isolated = private
        || captured
        || args.native_audit
        || ((args.headless.is_some() || args.first_frame) && args.value("--data-dir").is_none());
    if isolated {
        let profile = tempfile::Builder::new()
            .prefix("tilo-capture-")
            .tempdir()
            .map_err(super::headless::operation)?;
        let root = profile.path().join("state");
        let profile_root = profile.path().to_path_buf();
        let content = profile_content(profile.path());
        let mut host = ApplicationHost::new_with_profile_lifetime(
            root,
            "capture-device".into(),
            "capture-workspace",
            UserId("capture-author".into()),
            role_permissions("capture-author", Role::Owner),
            tilo_actions::ActionContext::default(),
            Rc::new(profile),
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
            // The theme the capture draws in is the stored choice its
            // controls show.
            ui_core::storage::SettingsDocument::File(super::headless::profile_settings(
                &profile_root,
            ))
            .update_section(
                ui_core::theme::APPEARANCE_SECTION,
                ui_core::launch::capture_appearance(args),
            )
            .map_err(super::headless::operation)?;
        }
        if private {
            prepared.context.provide_private();
        }
        return Ok(super::headless::in_profile(
            prepared,
            &profile_root,
            definition.app_id,
        ));
    }
    if let Some(directory) = args.value("--data-dir") {
        let author = ui_core::collaboration::local_user_id();
        let mut host = data_dir_host(
            std::path::Path::new(directory),
            &author,
            role_permissions(&author.0, Role::Owner),
        )?;
        return prepare_on(&mut host, definition, DATA_DIR_INSTANCE, named, inputs).map(
            |mut prepared| {
                prepared
                    .context
                    .provide_content(super::headless::data_dir_content(directory.as_ref()));
                super::headless::in_profile(
                    prepared,
                    &super::headless::data_dir_root(directory.as_ref()),
                    definition.app_id,
                )
            },
        );
    }
    let mut prepared = NativeProcessHost::prepare_window(definition, named, inputs)?;
    prepared
        .context
        .provide_content(tilo_content_device::for_app());
    Ok(prepared)
}

/// The content store of a profile the launch keeps in `directory`: the
/// bytes its records name live with them, and go with them.
#[cfg(tilo_native_runner)]
pub(crate) fn profile_content(directory: &std::path::Path) -> tilo_content::ContentStore {
    tilo_content::ContentStore::in_dir(directory.join("content"))
}

/// Take the launch's actions ([`ResourceApp::launch`]) as the live host's
/// first idle tick does, before anything else drives the harness.
#[cfg(tilo_native_runner)]
pub(crate) fn settle_launch(harness: &mut FrameHarness) {
    harness.wait_until(ui_core::time::Duration::ZERO, |_| true);
}

#[cfg(not(target_arch = "wasm32"))]
fn capture<A: ResourceApp>(
    harness: &mut FrameHarness,
    args: &LaunchArgs,
) -> Result<(), LaunchError> {
    settle_launch(harness);
    super::stage::stage::<A>(harness, args.scenario.as_deref())
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
    let spec = if A::OPENS_WITHOUT_IDENTITY {
        spec.runs_without_identity()
    } else {
        spec
    };
    let spec = if !A::ATTACHES_RUNTIME {
        spec.runtime_attachment(|_| false)
    } else if A::PRIVATE_WINDOWS {
        // A private window is nobody's runtime participant: it neither syncs
        // nor shares what it shows.
        spec.runtime_attachment(|args| !args.has(PRIVATE_FLAG))
    } else {
        spec
    };
    // An input surface types into other apps' fields: nothing drives it
    // without a person. It is a session service the compositor starts
    // without a room, so it has no workspace runtime to attach to.
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
/// instead of running: what `tilo-app surface` and `tilo-app check` read.
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
