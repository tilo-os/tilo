//! Declaring and running a framework app on the toolkit's component runtime.

use std::any::TypeId;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Mutex, OnceLock};

use tilo_resource::{Invoke, Resource};
use ui_core::application::{
    AppComponent, ApplicationDefinition, ApplicationHost, ComponentRunnerInputs, HostContext,
    LaunchTarget, NativeProcessHost, PreparedComponentInputs, RestoreInput,
};
use ui_core::bound_dispatch::BoundActionAddress;
use ui_core::headless::FrameHarness;
use ui_core::launch::{LaunchArgs, LaunchError, LaunchSpec};
use ui_core::permissions::{Actor, PermissionContext, PermissionScope, Role, UserId, Verb};
use ui_core::record::{RecordCheckout, RecordId, RecordKind};
use ui_core::widget::{Element, IconDescriptor};

use super::dispatch::Dispatcher;
use super::live::Live;
use super::root::ResourceRoot;
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

    /// Build the view. Called once for the live app, and again for every
    /// historical version History mode shows (with a read-only context), so
    /// it must build widgets and nothing else.
    fn view(cx: &mut ViewCx<Self::Resource>) -> Element;

    /// The label of the open resource in the palette and in history.
    fn label(state: &<Self::Resource as Resource>::State) -> String {
        let _ = state;
        <Self::Resource as Resource>::LABEL.to_owned()
    }

    /// Named screenshot scenarios for `--shots --scenario NAME`.
    fn scenarios() -> &'static [&'static str] {
        &[]
    }

    /// The actions that stage a screenshot scenario, applied through the
    /// same door as every control before the frame is captured.
    fn scenario(name: &str) -> Vec<Invoke<Self::Resource>> {
        let _ = name;
        Vec::new()
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

/// The record a standalone launch opens: one stable default per app and
/// workspace, created empty on first launch.
pub(crate) fn default_record<A: ResourceApp>() -> RecordCheckout {
    RecordCheckout::new(
        RecordId::derived("default", &RecordId::for_tab(0)),
        RecordKind::new(<A::Resource as Resource>::KIND),
    )
}

/// Build the framework root for a resource state: the live handle, the
/// dispatcher, the published resource and the app's view.
pub(crate) fn build_root<A: ResourceApp>(
    state: <A::Resource as Resource>::State,
    read_only: bool,
) -> ResourceRoot<A::Resource> {
    let label = A::label(&state);
    let resource_id = <A::Resource as Resource>::KIND;
    let published = tilo_resource::publish::<A::Resource>(resource_id, &label, &state);
    let live = Live::new(state);
    let updaters = Updaters::new();
    let alive = Rc::new(());
    let dispatch = Dispatcher::new(resource_id, read_only);
    let saved = ui_core::LocalState::new(ui_core::record::RecordSaveStatus::Saved);
    let mut cx = ViewCx::root(
        live.clone(),
        updaters.clone(),
        &alive,
        dispatch.clone(),
        published,
        saved.clone(),
    );
    let content = A::view(&mut cx);
    ResourceRoot::new(
        A::TITLE,
        icon::<A>(),
        label,
        live,
        updaters,
        alive,
        dispatch,
        saved,
        content,
    )
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
        context
            .initialize_new_record(|doc| {
                <A::Resource as Resource>::initialize(doc).map_err(|error| error.to_string())
            })
            .map_err(LaunchError::Record)?;
    }
    let read_only = !context
        .permissions()
        .check(&PermissionScope::app(A::ID, Verb::Write));
    let state = context
        .read_record(<A::Resource as Resource>::read)
        .map_err(LaunchError::Record)?
        .map_err(|error| LaunchError::Operation(error.to_string()))?;
    let root = build_root::<A>(state, read_only);
    let mut registry = tilo_actions::ActionRegistry::new();
    <A::Resource as Resource>::register(&mut registry);
    let mut component = AppComponent::new(context, Box::new(root), registry);
    component
        .register_document_history(super::preview::history_definition::<A>())
        .map_err(|error| LaunchError::Operation(error.to_string()))?;
    Ok(component)
}

const CAPTURE_INSTANCE: u64 = 1;
pub(crate) const DATA_DIR_INSTANCE: u64 = 0;

/// The host of an explicit `--data-dir` profile: one personal workspace in
/// that directory. Tools that seed or inspect such a profile open it the same
/// way the app does.
pub(crate) fn data_dir_host(
    directory: &std::path::Path,
    author: &UserId,
    role: Role,
) -> Result<ApplicationHost, LaunchError> {
    let root = std::path::absolute(directory).map_err(operation)?;
    ApplicationHost::new(
        root,
        "local-device".into(),
        "personal",
        author.clone(),
        Rc::new(RefCell::new(PermissionContext::from_capabilities(
            Actor::person(Some(role), &author.0),
            vec![],
        ))),
        tilo_actions::ActionContext::default(),
    )
}

/// Keeps a capture's temporary profile alive for as long as the host needs it.
struct CaptureProfile(#[allow(dead_code, reason = "held for its Drop")] tempfile::TempDir);

fn owner(author: &str) -> Rc<RefCell<PermissionContext>> {
    Rc::new(RefCell::new(PermissionContext::from_capabilities(
        Actor::person(Some(Role::Owner), author),
        vec![],
    )))
}

fn operation(error: impl std::fmt::Display) -> LaunchError {
    LaunchError::Operation(error.to_string())
}

fn prepare<A: ResourceApp>(
    definition: &ApplicationDefinition,
    args: &LaunchArgs,
) -> Result<PreparedComponentInputs, LaunchError> {
    let inputs = ComponentRunnerInputs::from_native_process();
    if args.shots.is_some() || args.responsive_matrix.is_some() || args.native_audit {
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
            owner("capture-author"),
            tilo_actions::ActionContext::default(),
            Rc::new(CaptureProfile(profile)),
        )?;
        return host.prepare_default(
            definition,
            CAPTURE_INSTANCE,
            default_record::<A>(),
            None,
            inputs,
        );
    }
    if let Some(directory) = args.value("--data-dir") {
        let author = ui_core::collaboration::local_user_id();
        let mut host = data_dir_host(std::path::Path::new(directory), &author, Role::Owner)?;
        return host.prepare_default(
            definition,
            DATA_DIR_INSTANCE,
            default_record::<A>(),
            None,
            inputs,
        );
    }
    let mut native = NativeProcessHost::open(definition)?;
    let instance = native.instance_id();
    native
        .host_mut()
        .prepare_default(definition, instance, default_record::<A>(), None, inputs)
}

fn capture<A: ResourceApp>(
    harness: &mut FrameHarness,
    args: &LaunchArgs,
) -> Result<(), LaunchError> {
    let Some(name) = args.scenario.as_deref() else {
        return Ok(());
    };
    for invoke in A::scenario(name) {
        let address = BoundActionAddress {
            instance_id: CAPTURE_INSTANCE,
            address: tilo_actions::ActionAddress::new(<A::Resource as Resource>::KIND, invoke.id),
        };
        harness
            .invoke_bound_action(&address, &invoke.arguments)
            .map_err(operation)?;
    }
    harness.relayout();
    Ok(())
}

fn shot_viewport<A: ResourceApp>(_: &LaunchArgs) -> (u32, u32) {
    A::SHOT_VIEWPORT
}

/// The launch specification of `A`, for apps that add options to it.
pub fn launch_spec<A: ResourceApp>() -> LaunchSpec {
    LaunchSpec::from_definition(definition::<A>())
        .component_host(prepare::<A>)
        .native_services()
        .capture_scenario(capture::<A>)
        .shot_viewport(shot_viewport::<A>)
        .scenarios(A::scenarios())
        .allowed_options(&["--data-dir"])
        .allowed_live_options(&["--data-dir"])
}

/// Run `A`: the live window, or `--shots`, `--headless` and the other
/// standard launch modes.
pub fn run<A: ResourceApp>() {
    ui_core::launch::run_or_exit(&launch_spec::<A>());
}
