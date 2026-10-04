//! Declaring and running a framework app on the toolkit's component runtime.

use std::any::TypeId;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Mutex, OnceLock};

use tilo_resource::{Invoke, Reader, Resource};
use ui_core::application::{
    AppComponent, ApplicationDefinition, ComponentBehaviour, HostContext, LaunchTarget,
    RestoreInput,
};
use ui_core::i18n::EmbeddedCatalog;
use ui_core::launch::LaunchError;
use ui_core::permissions::{LivePermissions, PermissionScope, Verb};
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
    ui_core::permissions::{Actor, PermissionContext, Role, UserId},
    ui_core::record::RecordCheckout,
};

use super::dispatch::Dispatcher;
use super::effect::Effects;
use super::launch::{Command, LaunchInput};
use super::live::Live;
use super::root::ResourceRoot;
use super::scenario::{Fixture, Run};
use super::view::ViewCx;

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
    /// shell and every other host of its component protect it alike.
    const BEHAVIOUR: ComponentBehaviour = ComponentBehaviour::DEFAULT;
    /// Media types (`type/subtype`, lowercase) of the files the app opens
    /// when a host hands it one (a file association, a file dropped on its
    /// icon, a path of that type on its command line); [`Self::launch`]
    /// receives the file.
    const OPENS: &'static [&'static str] = &[];
    /// How many arguments (paths, URLs) a launch may name;
    /// [`Self::launch_record`] and [`Self::launch`] receive them.
    const ARGUMENTS: usize = 0;

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
    /// participant's permissions, attributed, undoable and in history.
    fn launch(input: &LaunchInput<'_>, created: bool) -> Vec<Invoke<Self::Resource>> {
        let _ = (input, created);
        Vec::new()
    }

    /// The commands the app's binary runs instead of opening a window.
    fn commands() -> &'static [Command] {
        &[]
    }

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
        (1024, 720)
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
}

/// What [`ResourceApp::seed`] may read: who is creating the record, and the
/// app's host — its settings, files and secrets on this device.
pub struct SeedCx<'a> {
    context: &'a HostContext,
}

impl SeedCx<'_> {
    /// Who is creating the record: the id their views read as
    /// [`ViewCx::me`] and the seed's actions read as their author.
    pub fn me(&self) -> &str {
        &self.context.author().0
    }

    /// The app's host on this device.
    pub fn host(&self) -> &HostContext {
        self.context
    }
}

type Leaked = &'static (dyn std::any::Any + Send + Sync);

pub(crate) fn leaked<T: Send + Sync + 'static>(
    key: TypeId,
    make: impl FnOnce() -> T,
) -> &'static T {
    static TABLE: OnceLock<Mutex<HashMap<TypeId, Leaked>>> = OnceLock::new();
    let table = TABLE.get_or_init(Mutex::default);
    let mut table = table
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let entry = *table
        .entry(key)
        .or_insert_with(|| Box::leak(Box::new(make())) as Leaked);
    drop(table);
    // One value per (type, T) key is ever inserted, so the downcast holds.
    entry
        .downcast_ref::<T>()
        .unwrap_or_else(|| unreachable_definition())
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
    leaked(TypeId::of::<DefinitionKey<A>>(), || ApplicationDefinition {
        app_id: A::ID,
        title: A::TITLE,
        icon: icon::<A>(),
        record_kind: RecordKind::new(<A::Resource as Resource>::KIND),
        required: vec![],
        optional: vec![],
        opens: A::OPENS,
        create: create::<A>,
    })
}

fn icon<A: ResourceApp>() -> IconDescriptor {
    IconDescriptor::Named(
        A::ICON
            .unwrap_or_else(|| A::ID.rsplit('.').next().unwrap_or(A::ID))
            .to_owned(),
    )
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

/// The permission that makes an app's resource editable.
fn write_scope<A: ResourceApp>() -> PermissionScope {
    PermissionScope::app(A::ID, Verb::Write)
}

/// Build the framework root for a resource state: the live handle, the
/// dispatcher, the published resource and the app's view.
pub(crate) fn build_root<A: ResourceApp>(
    reader: Reader<A::Resource>,
    editing: Editing,
    storage: AppStorage,
) -> ResourceRoot<A::Resource> {
    let scope = write_scope::<A>();
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
    let label = A::label(reader.state());
    let resource_id = <A::Resource as Resource>::KIND;
    let published = tilo_resource::publish::<A::Resource>(resource_id, &label, reader.state());
    let live = Live::new(reader);
    let alive = Rc::new(());
    let dispatch = Dispatcher::new(A::ID, resource_id, read_only, me.as_deref());
    let storage = super::settings::ViewStorage::new(storage);
    // Effects and projection drains run in a window someone uses only:
    // History shows a past version, and a capture reaches nothing outside.
    let effects = (permissions.is_some() && run.mode.is_live())
        .then(|| Effects::live(dispatch.clone(), live.clone()));
    let mut cx = ViewCx::root(live, &alive, dispatch, published, run, storage);
    if let Some(effects) = effects {
        cx.effects = effects;
    }
    let content = A::view(&mut cx);
    let live = permissions.is_some();
    let root = ResourceRoot::new(A::TITLE, icon::<A>(), label, cx, alive, content)
        .following(permissions, scope)
        .with_presentation(A::local_operations());
    if live {
        root.drawing_past(super::preview::draw_past::<A>)
    } else {
        root
    }
}

/// The decoders of every edit `A`'s document takes: its resource's actions
/// and History's restores.
pub(crate) fn action_registry<A: ResourceApp>() -> tilo_actions::ActionRegistry {
    let mut registry = tilo_actions::ActionRegistry::new();
    tilo_resource::group::register::<A::Resource>(&mut registry);
    registry.register::<super::history::HistoryEdit>(super::history::EDIT_TYPE);
    registry
}

/// Apply a new record's [`ResourceApp::seed`] through the replayable
/// executor: attributed to `author` as their edits are, and recorded as
/// content from outside, which no undo takes back.
fn apply_seed<A: ResourceApp>(
    doc: &mut tilo_resource::Doc,
    author: &str,
    seed: &[Invoke<A::Resource>],
) -> Result<(), String> {
    let now_ms = tilo_resource::origin_now_ms();
    let mut actions = Vec::new();
    for invoke in seed {
        if invoke.is_group() {
            actions.extend(
                tilo_resource::group::build_group::<A::Resource>(&invoke.arguments, now_ms)?
                    .actions,
            );
        } else {
            actions.push(invoke.build(now_ms)?);
        }
    }
    let writer = ui_core::history_source::authors::device_actor(author, doc.get_actor());
    for action in &actions {
        let encoded = tilo_resource::encode::<A::Resource>(action).map_err(|e| e.to_string())?;
        let label = tilo_resource::action_label::<A::Resource>(action).unwrap_or_else(|| {
            tilo_actions::ActionContext::for_action("", "", action).action_label
        });
        let context = tilo_actions::ActionContext::for_resource(
            label,
            <A::Resource as Resource>::KIND,
            <A::Resource as Resource>::KIND,
            <A::Resource as Resource>::LABEL,
        )
        .with_app(A::ID, A::TITLE)
        .with_invoker(tilo_actions::ActionInvoker::actor(author))
        .with_change(tilo_actions::ChangeKind::External)
        .with_args_from_payload(&encoded.payload);
        tilo_actions::apply_local_with_context(doc, action, encoded, Some(&writer), context)
            .map_err(|error| format!("seeding the new record failed: {error}"))?;
    }
    Ok(())
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "the toolkit's ComponentFactory signature passes the target owned"
)]
fn create<A: ResourceApp>(
    mut context: HostContext,
    target: LaunchTarget,
    _restore: RestoreInput,
) -> Result<AppComponent, LaunchError> {
    if let Some(catalog) = A::catalog() {
        catalog.ensure();
    }
    let created = matches!(target, LaunchTarget::New);
    if created {
        let seed = A::seed(&SeedCx { context: &context });
        let author = context.author().0.clone();
        context
            .initialize_new_record_seeded(
                |doc| <A::Resource as Resource>::initialize(doc).map_err(|error| error.to_string()),
                |doc| apply_seed::<A>(doc, &author, &seed),
            )
            .map_err(LaunchError::Record)?;
    }
    let run = match context.capture().cloned() {
        Some(capture) => prepare_capture::<A>(&mut context, capture.scenario())?,
        None => Run::live(context.content().cloned()),
    };
    let permissions = context.live_permissions();
    let reader = context
        .read_record(Reader::<A::Resource>::read)
        .map_err(LaunchError::Record)?
        .map_err(|error| LaunchError::Operation(error.to_string()))?;
    let me = context.author().0.clone();
    let root = build_root::<A>(
        reader,
        Editing::Live {
            permissions,
            me,
            run,
        },
        context.storage(),
    );
    let file = context.take_open_file();
    let arguments = context.take_launch_arguments();
    if let Some(file) = &file {
        file.report_opened(<A::Resource as Resource>::KIND);
    }
    for invoke in A::launch(&LaunchInput::new(&arguments, file.as_ref()), created) {
        root.dispatcher().invoke(invoke);
    }
    let mut component = AppComponent::new(context, Box::new(root), action_registry::<A>());
    component.set_behaviour(A::BEHAVIOUR);
    component
        .register_document_history(super::preview::history_definition::<A>())
        .map_err(|error| LaunchError::Operation(error.to_string()))?;
    Ok(component)
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
        let mut peer = context
            .read_record(|doc| doc.clone().fork())
            .map_err(LaunchError::Record)?;
        fixture
            .write_history(&mut peer, &action_registry::<A>())
            .map_err(LaunchError::Operation)?;
        context
            .merge_peer_document(peer.save())
            .map_err(LaunchError::Operation)?;
    }
    Ok(fixture.into_run())
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
    let root = std::path::absolute(directory).map_err(operation)?;
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
    Rc::new(RefCell::new(PermissionContext::from_capabilities(
        Actor::person(Some(role), author),
        vec![],
    )))
}

#[cfg(not(target_arch = "wasm32"))]
/// Keeps a capture's temporary profile alive for as long as the host needs it.
struct CaptureProfile(#[allow(dead_code, reason = "held for its Drop")] tempfile::TempDir);

#[cfg(not(target_arch = "wasm32"))]
fn operation(error: impl std::fmt::Display) -> LaunchError {
    LaunchError::Operation(error.to_string())
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
        Some(id) => {
            host.prepare_record(definition, instance, RecordCheckout::new(id, kind), inputs)
        }
        None => host.prepare_default(
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
    let arguments = args.positionals();
    let named = A::launch_record(&LaunchInput::new(arguments, None));
    let mut prepared = prepare_named(definition, args, named)?;
    hand_launch(definition, arguments, &mut prepared)?;
    Ok(prepared)
}

/// Hand a prepared component what its launch named: the arguments, and the
/// file the first one names when the app opens its type.
///
/// # Errors
/// A file of a type the app opens that cannot be read.
#[cfg(tilo_native_runner)]
pub(crate) fn hand_launch(
    definition: &ApplicationDefinition,
    arguments: &[String],
    prepared: &mut PreparedComponentInputs,
) -> Result<(), LaunchError> {
    if let Some(path) = arguments.first().map(std::path::Path::new)
        && let Some(file) = launched_file(definition, path)?
    {
        prepared.context.provide_open_file(file);
    }
    prepared
        .context
        .provide_launch_arguments(arguments.to_vec());
    Ok(())
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
    let Some(media_type) = ui_core::dnd::mime_for_path(path)
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

#[cfg(not(target_arch = "wasm32"))]
fn prepare_definition(
    definition: &ApplicationDefinition,
    args: &LaunchArgs,
) -> Result<PreparedComponentInputs, LaunchError> {
    prepare_named(definition, args, None)
}

#[cfg(tilo_native_runner)]
fn prepare_named(
    definition: &ApplicationDefinition,
    args: &LaunchArgs,
    named: Option<RecordId>,
) -> Result<PreparedComponentInputs, LaunchError> {
    let inputs = ComponentRunnerInputs::from_native_process();
    // Captures, audits and headless driving never touch the user's profile:
    // they run on a private one removed with the process, unless a headless
    // drive names its profile with `--data-dir`.
    let isolated = args.shots.is_some()
        || args.responsive_matrix.is_some()
        || args.native_audit
        || (args.headless.is_some() && args.value("--data-dir").is_none());
    if isolated {
        let profile = tempfile::Builder::new()
            .prefix("tilo-capture-")
            .tempdir()
            .map_err(operation)?;
        let root = profile.path().join("state");
        let settings = profile.path().join("settings.json");
        let mut host = ApplicationHost::new_with_profile_lifetime(
            root,
            "capture-device".into(),
            "capture-workspace",
            UserId("capture-author".into()),
            role_permissions("capture-author", Role::Owner),
            tilo_actions::ActionContext::default(),
            Rc::new(CaptureProfile(profile)),
        )?;
        let mut prepared = prepare_on(&mut host, definition, CAPTURE_INSTANCE, named, inputs)?;
        // An audit and a headless drive certify and drive the app someone
        // uses; only a screenshot is built from fixtures.
        if args.shots.is_some() || args.responsive_matrix.is_some() {
            prepared
                .context
                .provide_capture(Capture::new(args.scenario.clone()));
        }
        return Ok(super::testing::with_settings_file(prepared, settings));
    }
    if let Some(directory) = args.value("--data-dir") {
        let author = ui_core::collaboration::local_user_id();
        let mut host = data_dir_host(
            std::path::Path::new(directory),
            &author,
            role_permissions(&author.0, Role::Owner),
        )?;
        return prepare_on(&mut host, definition, DATA_DIR_INSTANCE, named, inputs).map(
            |prepared| {
                super::testing::with_settings_file(
                    prepared,
                    super::testing::data_dir_settings(directory.as_ref()),
                )
            },
        );
    }
    let mut native = NativeProcessHost::open(definition)?;
    let instance = native.instance_id();
    prepare_on(native.host_mut(), definition, instance, named, inputs)
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

/// The options a live launch of `A` takes: its profile and its commands.
#[cfg(tilo_native_runner)]
fn live_options<A: ResourceApp>() -> &'static [&'static str] {
    leaked(TypeId::of::<LiveOptionsKey<A>>(), || {
        let mut options = vec!["--data-dir"];
        options.extend(A::commands().iter().map(|command| command.option));
        options
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

#[cfg(tilo_native_runner)]
/// The launch specification of `A`, for apps that add options to it.
pub fn launch_spec<A: ResourceApp>() -> LaunchSpec {
    LaunchSpec::from_definition(definition::<A>())
        .component_host(prepare_app::<A>)
        .native_services()
        .capture_scenario(capture::<A>)
        .shot_viewport(shot_viewport::<A>)
        .scenarios(A::scenarios())
        .max_positionals(A::ARGUMENTS)
        .command_handler(run_command::<A>)
        .allowed_options(&["--data-dir"])
        .allowed_live_options(live_options::<A>())
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
    crate::launch::run_or_exit(&launch_spec::<A>());
}
