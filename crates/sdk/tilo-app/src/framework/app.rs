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
use ui_core::launch::LaunchError;
use ui_core::permissions::{LivePermissions, PermissionScope, Verb};
use ui_core::record::RecordKind;
use ui_core::widget::{Element, IconDescriptor};

// Launching (live windows, captures, headless drives, test hosts) discovers
// native presentation and profiles; a browser host prepares components itself.
#[cfg(not(target_arch = "wasm32"))]
use {
    std::cell::RefCell,
    ui_core::application::{
        ApplicationHost, ComponentRunnerInputs, NativeProcessHost, PreparedComponentInputs,
    },
    ui_core::headless::FrameHarness,
    ui_core::launch::{LaunchArgs, LaunchSpec},
    ui_core::permissions::{Actor, PermissionContext, Role, UserId},
    ui_core::record::{RecordCheckout, RecordId},
};

use super::dispatch::Dispatcher;
use super::live::Live;
use super::root::ResourceRoot;
use super::stage::Stages;
use super::view::{Updaters, ViewCx};

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
    /// The `--shots` viewport.
    const SHOT_VIEWPORT: (u32, u32) = (1024, 720);
    /// How the hosting window treats the app's tree.
    const BEHAVIOUR: ComponentBehaviour = ComponentBehaviour {
        autofocus_on_structure_change: false,
        appearance_reconcile: false,
    };

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

    /// Named screenshot scenarios for `--shots --scenario NAME`.
    fn scenarios() -> &'static [&'static str] {
        &[]
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
        opens: &[],
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

#[cfg(not(target_arch = "wasm32"))]
/// The record a standalone launch opens: one stable default per app and
/// workspace, created empty on first launch.
pub(crate) fn default_record<A: ResourceApp>() -> RecordCheckout {
    RecordCheckout::new(
        RecordId::derived("default", &RecordId::for_tab(0)),
        RecordKind::new(<A::Resource as Resource>::KIND),
    )
}

/// Who may edit the resource a root shows.
pub(crate) enum Editing {
    /// The participant (`me`) and their permissions, followed while the app
    /// runs.
    Live {
        permissions: LivePermissions,
        me: String,
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
) -> ResourceRoot<A::Resource> {
    let scope = write_scope::<A>();
    let (permissions, read_only, me) = match editing {
        Editing::Live { permissions, me } => {
            let read_only = !permissions.check(&scope);
            (Some(permissions), read_only, Some(me))
        }
        Editing::ReadOnly => (None, true, None),
    };
    let label = A::label(reader.state());
    let resource_id = <A::Resource as Resource>::KIND;
    let published = tilo_resource::publish::<A::Resource>(resource_id, &label, reader.state());
    let live = Live::new(reader);
    let updaters = Updaters::new();
    let alive = Rc::new(());
    let dispatch = Dispatcher::new(A::ID, resource_id, read_only, me.as_deref());
    let saved = ui_core::LocalState::new(ui_core::record::RecordSaveStatus::Saved);
    let stages = Stages::new();
    let mut cx = ViewCx::root(live, updaters, &alive, dispatch, published, saved, stages);
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
    if matches!(target, LaunchTarget::New) {
        let seed = A::seed(&SeedCx { context: &context });
        let author = context.author().0.clone();
        context
            .initialize_new_record_seeded(
                |doc| <A::Resource as Resource>::initialize(doc).map_err(|error| error.to_string()),
                |doc| apply_seed::<A>(doc, &author, &seed),
            )
            .map_err(LaunchError::Record)?;
    }
    let permissions = context.live_permissions();
    let reader = context
        .read_record(Reader::<A::Resource>::read)
        .map_err(LaunchError::Record)?
        .map_err(|error| LaunchError::Operation(error.to_string()))?;
    let me = context.author().0.clone();
    let root = build_root::<A>(reader, Editing::Live { permissions, me });
    let mut component = AppComponent::new(context, Box::new(root), action_registry::<A>());
    component.set_behaviour(A::BEHAVIOUR);
    component
        .register_document_history(super::preview::history_definition::<A>())
        .map_err(|error| LaunchError::Operation(error.to_string()))?;
    Ok(component)
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

/// The record a standalone launch opens: the default one of the definition's kind.
#[cfg(not(target_arch = "wasm32"))]
fn default_record_of(definition: &ApplicationDefinition) -> RecordCheckout {
    RecordCheckout::new(RecordId::room_default(), definition.record_kind.clone())
}

#[cfg(not(target_arch = "wasm32"))]
fn prepare_definition(
    definition: &ApplicationDefinition,
    args: &LaunchArgs,
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
        let mut host = ApplicationHost::new_with_profile_lifetime(
            root,
            "capture-device".into(),
            "capture-workspace",
            UserId("capture-author".into()),
            role_permissions("capture-author", Role::Owner),
            tilo_actions::ActionContext::default(),
            Rc::new(CaptureProfile(profile)),
        )?;
        return host.prepare_default(
            definition,
            CAPTURE_INSTANCE,
            default_record_of(definition),
            None,
            inputs,
        );
    }
    if let Some(directory) = args.value("--data-dir") {
        let author = ui_core::collaboration::local_user_id();
        let mut host = data_dir_host(
            std::path::Path::new(directory),
            &author,
            role_permissions(&author.0, Role::Owner),
        )?;
        return host.prepare_default(
            definition,
            DATA_DIR_INSTANCE,
            default_record_of(definition),
            None,
            inputs,
        );
    }
    let mut native = NativeProcessHost::open(definition)?;
    let instance = native.instance_id();
    native.host_mut().prepare_default(
        definition,
        instance,
        default_record_of(definition),
        None,
        inputs,
    )
}

#[cfg(not(target_arch = "wasm32"))]
fn capture<A: ResourceApp>(
    harness: &mut FrameHarness,
    args: &LaunchArgs,
) -> Result<(), LaunchError> {
    super::stage::stage::<A>(harness, args.scenario.as_deref())
}

#[cfg(not(target_arch = "wasm32"))]
fn shot_viewport<A: ResourceApp>(_: &LaunchArgs) -> (u32, u32) {
    A::SHOT_VIEWPORT
}

#[cfg(tilo_native_runner)]
/// The launch specification of `A`, for apps that add options to it.
pub fn launch_spec<A: ResourceApp>() -> LaunchSpec {
    LaunchSpec::from_definition(definition::<A>())
        .component_host(prepare_definition)
        .native_services()
        .capture_scenario(capture::<A>)
        .shot_viewport(shot_viewport::<A>)
        .scenarios(A::scenarios())
        .allowed_options(&["--data-dir"])
        .allowed_live_options(&["--data-dir"])
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
