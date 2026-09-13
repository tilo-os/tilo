//! Test-only application definition. Every persistence and authority input is explicit.
use automerge::{AutoCommit, ObjId, ROOT, ReadDoc, transaction::Transactable};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use ui::application::{
    AppComponent, ApplicationDefinition, HostContext, LaunchTarget, RestoreInput,
};
use ui::application_view::InstanceViewStore;
use ui::launch::LaunchError;
use ui::local_state::LocalState;
use ui::permissions::{Actor, PermissionContext, PermissionScope, UserId, Verb};
use ui::record::{RecordCheckout, RecordKind, RecordOwners, RecordStore};
use ui::view::ViewContribution;
use ui::wake::WakeHandle;
use ui::widget::{AppKind, AppWidget, IconDescriptor, Widget};

pub const APP: &str = "dev.qos.application-probe";
pub const DEVICE: &str = "D";
pub const ROOM: &str = "A";
thread_local! {
    static CALLS: Cell<usize> = const { Cell::new(0) };
    static INITIALIZATIONS: Cell<usize> = const { Cell::new(0) };
    static HANDLES: RefCell<Vec<WakeHandle>> = const { RefCell::new(vec![]) };
    static LOCAL: RefCell<Option<LocalState<String>>> = const { RefCell::new(None) };
    static PANIC_ON_DROP: Cell<bool> = const { Cell::new(false) };
    static DROP_CLOSED: Cell<bool> = const { Cell::new(false) };
    static FOREIGN_CONTEXT: RefCell<Option<HostContext>> = const { RefCell::new(None) };
    static OPERATIONS: RefCell<Vec<Rc<RefCell<Vec<String>>>>> = const { RefCell::new(vec![]) };
    static PROVIDERS: RefCell<Option<Rc<RefCell<ProviderObservations>>>> = const { RefCell::new(None) };
}

#[derive(Default)]
pub struct ProviderObservations {
    pub methods: Vec<(&'static str, bool)>,
    pub drops: Vec<(bool, bool)>,
    pub panic_on_drop: bool,
}
pub fn enable_provider_probe() -> Rc<RefCell<ProviderObservations>> {
    let observations = Rc::new(RefCell::new(ProviderObservations::default()));
    PROVIDERS.with_borrow_mut(|providers| *providers = Some(observations.clone()));
    observations
}
struct ProbeProvider {
    observations: Rc<RefCell<ProviderObservations>>,
    local: LocalState<bool>,
}
impl ProbeProvider {
    fn observe(&self, method: &'static str) {
        self.observations
            .borrow_mut()
            .methods
            .push((method, WakeHandle::new().is_active()));
        self.local.writer().try_set(true);
    }
    fn hit() -> ui::search::SearchResult {
        ui::search::SearchResult::for_action(
            "Probe",
            "Probe",
            qos_actions::ActionAddress::new("probe:shared", "edit"),
        )
    }
}
impl ui::search::SearchProvider for ProbeProvider {
    fn name(&self) -> &'static str {
        self.observe("name");
        "Probe"
    }
    fn action_directory_generation(&self) -> Option<u64> {
        self.observe("generation");
        Some(4)
    }
    fn action_directory_cursor_is_current(&self, _: &ui::search::SearchCursor) -> bool {
        self.observe("cursor");
        true
    }
    fn search(&self, _: &str) -> Vec<ui::search::SearchResult> {
        self.observe("search");
        vec![Self::hit()]
    }
    fn search_limited(&self, _: &str, limit: usize) -> Vec<ui::search::SearchResult> {
        self.observe("limited");
        if limit > 0 { vec![Self::hit()] } else { vec![] }
    }
    fn search_page(
        &self,
        _: &str,
        _: Option<&ui::search::SearchCursor>,
        work: usize,
        limit: usize,
    ) -> ui::search::SearchPage {
        self.observe("page");
        ui::search::SearchPage {
            results: if work > 0 && limit > 0 {
                vec![Self::hit()]
            } else {
                vec![]
            },
            next: Some(ui::search::SearchCursor::default()),
            inspected: work.min(limit).min(1),
        }
    }
    fn resolve_action_resource(
        &self,
        _: &qos_actions::ActionAddress,
    ) -> Option<qos_actions::Resource> {
        self.observe("resolve");
        None
    }
}
impl Drop for ProbeProvider {
    fn drop(&mut self) {
        let mut observations = self.observations.borrow_mut();
        observations.drops.push((
            WakeHandle::new().is_active(),
            self.local.writer().try_set(false),
        ));
        assert!(
            !std::mem::take(&mut observations.panic_on_drop),
            "controlled provider destructor panic"
        );
    }
}
pub fn panic_on_next_root_drop() {
    PANIC_ON_DROP.set(true);
}
pub fn drop_was_closed() -> bool {
    DROP_CLOSED.get()
}
pub fn calls() -> usize {
    CALLS.get()
}
pub fn initializations() -> usize {
    INITIALIZATIONS.get()
}
pub fn captured_handles() -> Vec<WakeHandle> {
    HANDLES.with_borrow(Clone::clone)
}
pub fn local_state() -> LocalState<String> {
    LOCAL.with_borrow(|v| v.clone().unwrap())
}
pub fn permissions() -> Rc<RefCell<PermissionContext>> {
    Rc::new(RefCell::new(
        PermissionContext::from_capabilities(Actor::person(None, "probe-author"), vec![])
            .at_unix_time(100),
    ))
}
pub fn author() -> UserId {
    UserId("probe-author".into())
}
pub fn provenance() -> qos_actions::ActionContext {
    qos_actions::ActionContext::default().with_app(APP, "Probe")
}
pub fn denied_scope() -> PermissionScope {
    PermissionScope::workspace(Verb::Write)
}
pub fn definition(create: ui::application::ComponentFactory) -> ApplicationDefinition {
    ApplicationDefinition {
        app_id: APP,
        title: "Probe",
        icon: IconDescriptor::Glyph("P"),
        record_kind: RecordKind::new("probe"),
        required: vec![],
        optional: vec![],
        create,
    }
}
pub fn probe_definition() -> &'static ApplicationDefinition {
    static DEFINITION: std::sync::OnceLock<ApplicationDefinition> = std::sync::OnceLock::new();
    DEFINITION.get_or_init(|| definition(probe_factory))
}
pub struct ProbeHost {
    pub temp: tempfile::TempDir,
    pub owners: RecordOwners,
    pub records: RecordStore,
    pub views: Rc<InstanceViewStore>,
}
impl ProbeHost {
    pub fn new() -> Self {
        CALLS.set(0);
        OPERATIONS.with_borrow_mut(Vec::clear);
        INITIALIZATIONS.set(0);
        HANDLES.with_borrow_mut(Vec::clear);
        PROVIDERS.with_borrow_mut(|providers| *providers = None);
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("state");
        let mut owners = RecordOwners::new(root.clone());
        let records = owners.open(APP, ROOM).unwrap();
        let views = Rc::new(InstanceViewStore::new(root, DEVICE, ROOM).unwrap());
        Self {
            temp,
            owners,
            records,
            views,
        }
    }
    pub fn root(&self) -> std::path::PathBuf {
        self.temp.path().join("state")
    }
    pub fn view_path(&self, instance: u64) -> std::path::PathBuf {
        self.root()
            .join("instance-views/44/41")
            .join(format!("{instance}.postcard"))
    }
    pub fn prepare(
        &self,
        definition: &ApplicationDefinition,
        instance: u64,
        target: &LaunchTarget,
    ) -> Result<HostContext, LaunchError> {
        self.prepare_with_permissions(definition, instance, target, permissions())
    }
    pub fn prepare_with_permissions(
        &self,
        definition: &ApplicationDefinition,
        instance: u64,
        target: &LaunchTarget,
        permissions: Rc<RefCell<PermissionContext>>,
    ) -> Result<HostContext, LaunchError> {
        HostContext::prepare(
            definition,
            self.records.clone(),
            self.views.clone(),
            DEVICE.into(),
            instance,
            permissions,
            author(),
            provenance(),
            target,
        )
    }
}
pub fn instantiate_probe(
    host: &ProbeHost,
    instance: u64,
    target: LaunchTarget,
    restore: RestoreInput,
) -> Result<AppComponent, LaunchError> {
    let context = host.prepare(probe_definition(), instance, &target)?;
    probe_definition().instantiate(context, target, restore)
}
pub fn existing(checkout: &RecordCheckout) -> LaunchTarget {
    LaunchTarget::Record {
        checkout: checkout.clone(),
        reveal: None,
    }
}
pub fn body(records: &RecordStore, checkout: &RecordCheckout) -> String {
    records
        .read(&checkout.id, |doc| {
            doc.get(ROOT, "body")
                .unwrap()
                .unwrap()
                .0
                .to_str()
                .unwrap()
                .to_owned()
        })
        .unwrap()
}
#[allow(
    clippy::panic_in_result_fn,
    reason = "test assertions verify the once-only initializer contract"
)]
fn initialize(context: &mut HostContext, target: &LaunchTarget) -> Result<(), LaunchError> {
    HANDLES.with_borrow_mut(|v| v.push(context.wake_handle()));
    if matches!(target, LaunchTarget::New) {
        context
            .initialize_new_record(|doc| {
                INITIALIZATIONS.set(initializations().saturating_add(1));
                doc.put(ROOT, "body", "initialized")
                    .map_err(|e| e.to_string())?;
                Ok(())
            })
            .map_err(LaunchError::Record)?;
        assert!(
            context
                .initialize_new_record(|_| panic!("second initializer"))
                .is_err()
        );
    } else {
        assert!(
            context
                .initialize_new_record(|_| panic!("existing initializer"))
                .is_err()
        );
    }
    Ok(())
}
#[allow(
    clippy::needless_pass_by_value,
    reason = "ComponentFactory has an owned target signature"
)]
pub fn probe_factory(
    context: HostContext,
    target: LaunchTarget,
    _: RestoreInput,
) -> Result<AppComponent, LaunchError> {
    build_probe(context, &target, false)
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "ComponentFactory owned signature"
)]
pub fn action_probe_factory(
    context: HostContext,
    target: LaunchTarget,
    _: RestoreInput,
) -> Result<AppComponent, LaunchError> {
    build_probe(context, &target, true)
}
pub fn operation_observations() -> Vec<Rc<RefCell<Vec<String>>>> {
    OPERATIONS.with_borrow(Clone::clone)
}
fn build_probe(
    mut context: HostContext,
    target: &LaunchTarget,
    operational: bool,
) -> Result<AppComponent, LaunchError> {
    CALLS.set(calls().saturating_add(1));
    initialize(&mut context, target)?;
    let body = context
        .read_record(|doc| {
            doc.get(ROOT, "body")
                .unwrap()
                .unwrap()
                .0
                .to_str()
                .unwrap()
                .to_owned()
        })
        .map_err(LaunchError::Record)?;
    let local = LocalState::new(body);
    local.writer().set("construction wake".into());
    LOCAL.with_borrow_mut(|v| *v = Some(local.clone()));
    let mut registry = qos_actions::ActionRegistry::new();
    registry.register::<ProbeAction>("probe.edit");
    let operations = Rc::new(RefCell::new(Vec::new()));
    if operational {
        OPERATIONS.with_borrow_mut(|all| all.push(operations.clone()));
    }
    Ok(AppComponent::new(
        context,
        Box::new(ProbeRoot {
            operational,
            operations,
            local,
            scroll: vec![],
            caret: String::new(),
            focused: false,
            corrupt_restore: false,
        }),
        registry,
    ))
}
#[allow(
    clippy::needless_pass_by_value,
    reason = "ComponentFactory has an owned target signature"
)]
pub fn fail_after_initialization(
    mut context: HostContext,
    target: LaunchTarget,
    _: RestoreInput,
) -> Result<AppComponent, LaunchError> {
    initialize(&mut context, &target)?;
    Err(LaunchError::Operation("factory refused".into()))
}
#[allow(
    clippy::unnecessary_wraps,
    reason = "ComponentFactory requires Result even when the test deliberately omits initialization"
)]
pub fn omit_initialization(
    context: HostContext,
    _: LaunchTarget,
    _: RestoreInput,
) -> Result<AppComponent, LaunchError> {
    HANDLES.with_borrow_mut(|v| v.push(context.wake_handle()));
    Ok(AppComponent::new(
        context,
        Box::new(ProbeRoot {
            operational: false,
            operations: Rc::default(),
            local: LocalState::new(String::new()),
            scroll: vec![],
            caret: String::new(),
            focused: false,
            corrupt_restore: false,
        }),
        qos_actions::ActionRegistry::new(),
    ))
}
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct ProbeAction(pub String);
impl qos_actions::Action for ProbeAction {
    fn apply(&self, doc: &mut AutoCommit) -> Result<(), qos_actions::ActionError> {
        doc.put(ROOT, "body", self.0.clone())?;
        Ok(())
    }
}
struct ProbeRoot {
    operational: bool,
    operations: Rc<RefCell<Vec<String>>>,
    local: LocalState<String>,
    scroll: Vec<f64>,
    caret: String,
    focused: bool,
    corrupt_restore: bool,
}
impl Widget for ProbeRoot {
    fn resources(&self, _: &AutoCommit) -> Vec<qos_actions::Resource> {
        if !self.operational {
            return vec![];
        }
        vec![
            qos_actions::Resource::new("probe:shared", "probe", "Shared probe")
                .with_action(
                    qos_actions::ActionSpec::immediate(
                        "edit",
                        "Edit",
                        "probe.edit",
                        &ProbeAction("edited".into()),
                    )
                    .unwrap()
                    .requiring(qos_actions::PermissionRequirement::Write),
                )
                .with_action(
                    qos_actions::ActionSpec::operation(
                        "view",
                        "View",
                        qos_actions::OperationKind::View,
                        "probe.view",
                        &(),
                    )
                    .unwrap()
                    .with_contract(qos_actions::ActionContract::view()),
                ),
        ]
    }
    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &["probe.view"]
    }
    fn start_resource_operation_with_document(
        &mut self,
        _: qos_actions::InvocationId,
        kind: qos_actions::OperationKind,
        type_id: &str,
        _: &[u8],
        _: &mut AutoCommit,
        _: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        if type_id != "probe.view" {
            return Ok(None);
        }
        self.operations.borrow_mut().push(format!("{kind:?}"));
        Ok(Some(ui::widget::ResourceOperationStart::Completed(
            qos_actions::ActionOutput::default(),
        )))
    }
    fn search_providers(&self) -> Vec<Box<dyn ui::search::SearchProvider>> {
        PROVIDERS.with_borrow(|providers| {
            providers.as_ref().map_or_else(Vec::new, |observations| {
                vec![Box::new(ProbeProvider {
                    observations: observations.clone(),
                    local: LocalState::new(false),
                }) as Box<dyn ui::search::SearchProvider>]
            })
        })
    }
    fn sync(&mut self, _: &mut AutoCommit, _: &ObjId) {
        assert!(self.operational, "construction must project read-only");
    }
    fn build_layout(
        &mut self,
        tree: &mut taffy::TaffyTree<()>,
        _: &ui::Theme,
        map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        let node = tree.new_leaf(taffy::Style::default()).unwrap();
        map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }
    fn apply_layout(&mut self, _: &taffy::TaffyTree<()>, _: taffy::NodeId, _: vello::kurbo::Point) {
    }
    fn draw(
        &mut self,
        _: &mut vello::Scene,
        _: vello::kurbo::Affine,
        _: &mut ui::TextRenderer,
        _: &ui::Theme,
        _: &ui::PresenceCtx,
    ) {
    }
    fn handle_event(
        &mut self,
        _: &winit::event::WindowEvent,
        _: f64,
        _: &winit::event::Modifiers,
        _: &mut AutoCommit,
        _: &ObjId,
        _: &mut bool,
        _: &mut Option<taffy::NodeId>,
        _: taffy::NodeId,
        _: &taffy::TaffyTree<()>,
    ) {
    }
    fn name(&self) -> &'static str {
        "application-probe"
    }
    fn get_rect(&self) -> vello::kurbo::Rect {
        vello::kurbo::Rect::ZERO
    }
    fn is_focusable(&self) -> bool {
        true
    }
    fn is_focused(&self) -> bool {
        self.focused
    }
    fn set_focus(&mut self, focused: bool) {
        self.focused = focused;
    }
    fn get_state(&self) -> Option<String> {
        Some(self.caret.clone())
    }
    fn set_state(&mut self, state: &str) {
        self.caret = state.into();
    }
    fn view_state(&self) -> Option<ViewContribution> {
        Some(ViewContribution {
            scroll: self.scroll.clone(),
            view_mode: None,
        })
    }
    fn restore_view_state(&mut self, view: &ViewContribution) {
        self.scroll.clone_from(&view.scroll);
        if self.corrupt_restore {
            self.scroll = vec![f64::NAN];
        }
    }
}
impl AppWidget for ProbeRoot {
    fn app_kind(&self) -> AppKind {
        AppKind::Editor
    }
    fn app_title(&self) -> &'static str {
        "Probe"
    }
    fn app_icon(&self) -> IconDescriptor {
        IconDescriptor::Glyph("P")
    }
    fn permission_hint(&self) -> Option<AppKind> {
        None
    }
}

#[allow(
    clippy::panic_in_result_fn,
    reason = "test assertion verifies a factory cannot mount before validation"
)]
pub fn attempt_premature_mount(
    context: HostContext,
    target: LaunchTarget,
    restore: RestoreInput,
) -> Result<AppComponent, LaunchError> {
    let mut component = probe_factory(context, target, restore)?;
    assert!(
        component
            .mount(|_| panic!("unvalidated component woke a host"))
            .is_err()
    );
    Ok(component)
}
#[allow(
    clippy::needless_pass_by_value,
    reason = "ComponentFactory has an owned target signature"
)]
pub fn invalid_contribution(
    mut context: HostContext,
    target: LaunchTarget,
    _: RestoreInput,
) -> Result<AppComponent, LaunchError> {
    initialize(&mut context, &target)?;
    Ok(AppComponent::new(
        context,
        Box::new(ProbeRoot {
            operational: false,
            operations: Rc::default(),
            local: LocalState::new(String::new()),
            scroll: vec![f64::INFINITY],
            caret: String::new(),
            focused: false,
            corrupt_restore: false,
        }),
        qos_actions::ActionRegistry::new(),
    ))
}

impl Drop for ProbeRoot {
    fn drop(&mut self) {
        let captured_during_drop = WakeHandle::new();
        DROP_CLOSED.set(!captured_during_drop.is_active() && !self.local.try_set("drop".into()));
        if PANIC_ON_DROP.replace(false) {
            std::panic::panic_any("probe destructor panic");
        }
    }
}
#[allow(
    clippy::needless_pass_by_value,
    reason = "ComponentFactory has an owned target signature"
)]
pub fn restore_breaks_contribution(
    mut context: HostContext,
    target: LaunchTarget,
    _: RestoreInput,
) -> Result<AppComponent, LaunchError> {
    initialize(&mut context, &target)?;
    Ok(AppComponent::new(
        context,
        Box::new(ProbeRoot {
            operational: false,
            operations: Rc::default(),
            local: LocalState::new(String::new()),
            scroll: vec![],
            caret: String::new(),
            focused: false,
            corrupt_restore: true,
        }),
        qos_actions::ActionRegistry::new(),
    ))
}

pub fn set_foreign_context(context: HostContext) {
    FOREIGN_CONTEXT.with_borrow_mut(|slot| *slot = Some(context));
}
pub fn foreign_component(
    mut context: HostContext,
    target: LaunchTarget,
    restore: RestoreInput,
) -> Result<AppComponent, LaunchError> {
    initialize(&mut context, &target)?;
    drop(context);
    let foreign = FOREIGN_CONTEXT.with_borrow_mut(Option::take).unwrap();
    probe_factory(foreign, target, restore)
}
pub fn initializer_failure(
    mut context: HostContext,
    _: LaunchTarget,
    _: RestoreInput,
) -> Result<AppComponent, LaunchError> {
    HANDLES.with_borrow_mut(|handles| handles.push(context.wake_handle()));
    context
        .initialize_new_record(|doc| {
            doc.put(ROOT, "body", "unpublished")
                .map_err(|e| e.to_string())?;
            Err("schema initialization refused".into())
        })
        .map_err(LaunchError::Record)?;
    Err(LaunchError::Operation(
        "initializer unexpectedly succeeded".into(),
    ))
}
