//! Test-only application definition. Every persistence and authority input is explicit.
use automerge::{AutoCommit, ObjId, ROOT, ReadDoc, transaction::Transactable};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use tilo_ui_base::TextRole;
use tilo_ui::application::{
    AppComponent, ApplicationDefinition, LaunchTarget, ProcessContext, ProcessContextInputs,
    RestoreInput,
};
use tilo_ui::application_view::{InstanceViewStore, SavedInstanceView};
use tilo_ui::launch::LaunchError;
use tilo_ui_base::local_state::LocalState;
use tilo_ui_base::{permissions::ActionKind, permissions::MemberId, permissions::PermissionContext, permissions::PermissionScope, permissions::SharedPermissions};
use tilo_ui::resource::{
    DocumentAsk, DocumentCheckout, DocumentPort, DocumentRead, MemoryWorkspace, ResourceOwners,
    ResourceStore, ResourceType, Written,
};
use tilo_ui::view::ViewContribution;
use tilo_ui::wake::WakeHandle;
use tilo_ui::{widget::AppKind, widget::AppWidget, widget::IconDescriptor};
use tilo_ui_base::{widget::NativeViewLifecycle, widget::Widget};
use tilo_ui_base::{widget::AppModel, widget::ModelPath};

pub const APP: &str = "so.tilo.application-probe";
pub const DEVICE: &str = "D";
/// The workspace the probe's records and views live in: a workspace names a cluster, so a port
/// that answers for it names it as one.
pub const WORKSPACE: &str = "00000000-0000-4000-8000-000000000041";
thread_local! {
    static CALLS: Cell<usize> = const { Cell::new(0) };
    static INITIALIZATIONS: Cell<usize> = const { Cell::new(0) };
    static HANDLES: RefCell<Vec<WakeHandle>> = const { RefCell::new(vec![]) };
    static LOCAL: RefCell<Option<LocalState<String>>> = const { RefCell::new(None) };
    static PANIC_ON_DROP: Cell<bool> = const { Cell::new(false) };
    static DROP_CLOSED: Cell<bool> = const { Cell::new(false) };
    static FOREIGN_CONTEXT: RefCell<Option<ProcessContext>> = const { RefCell::new(None) };
    static OPERATIONS: RefCell<Vec<Rc<RefCell<Vec<String>>>>> = const { RefCell::new(vec![]) };
    static PROVIDERS: RefCell<Option<Rc<RefCell<ProviderObservations>>>> = const { RefCell::new(None) };
    static PRESENTATION: RefCell<Option<Rc<RefCell<PresentationObservations>>>> = const { RefCell::new(None) };
    static NATIVE_LIFECYCLE: RefCell<Vec<(&'static str, WakeHandle)>> = const { RefCell::new(vec![]) };
}

pub fn take_native_lifecycle_observations() -> Vec<(&'static str, WakeHandle)> {
    NATIVE_LIFECYCLE.with_borrow_mut(std::mem::take)
}

#[derive(Default)]
pub struct PresentationObservations {
    pub layouts: Vec<tilo_ui_base::Theme>,
    pub draws: Vec<(tilo_ui_base::Theme, f32)>,
    pub motion: Vec<bool>,
    pub local_draws: Vec<String>,
    pub local_resource_reads: Vec<String>,
}
pub fn observe_presentation() -> Rc<RefCell<PresentationObservations>> {
    let observations = Rc::new(RefCell::new(PresentationObservations::default()));
    PRESENTATION.with_borrow_mut(|slot| *slot = Some(observations.clone()));
    observations
}
/// An effective snapshot with intentionally distinct type and runner factors.
pub fn presentation_inputs(theme_scale: f32) -> tilo_ui::application::ComponentRunnerInputs {
    configure_presentation(runner_inputs(), theme_scale)
}
pub fn configure_presentation(
    mut inputs: tilo_ui::application::ComponentRunnerInputs,
    theme_scale: f32,
) -> tilo_ui::application::ComponentRunnerInputs {
    inputs.theme.name = "explicit presentation".into();
    inputs.theme.tokens.typography = inputs.theme.tokens.typography.clone().with_base_size(19.0);
    inputs.theme.tokens.colors.intent.accent = inputs.theme.status_success;
    inputs.theme = inputs
        .theme
        .with_ui_scale(1.25)
        .with_text_scale(theme_scale)
        .with_reduce_motion(true);
    inputs.theme.radius_standard = 17.5;
    inputs.theme.padding_standard = 21.25;
    inputs.theme.gap_standard = 8.75;
    inputs.text_renderer.set_text_scale(1.5);
    inputs
}
pub fn assert_presentation(theme: &tilo_ui_base::Theme, scale: f32, ui_scale: f32) {
    assert_eq!(theme.text_scale, scale);
    assert_eq!(theme.ui_scale, ui_scale);
    assert_eq!(theme.name, "explicit presentation");
    assert!(theme.reduce_motion);
    assert_eq!(theme.tokens.colors.intent.accent, theme.status_success);
    assert!(
        (theme.tokens.typography.size(TextRole::Body) - 19.0 * ui_scale * scale).abs() < 0.0001
    );
    let base = tilo_ui_base::Theme::dark();
    assert!(
        (theme.tokens.typography.size(TextRole::Caption)
            - base.tokens.typography.size(TextRole::Caption) * 19.0 / 14.0 * ui_scale * scale)
            .abs()
            < 0.0001
    );
    assert!(
        (theme.tokens.typography.size(TextRole::Display)
            - base.tokens.typography.size(TextRole::Display) * 19.0 / 14.0 * ui_scale * scale)
            .abs()
            < 0.001
    );
    assert!((theme.tokens.spacing.s3 - base.tokens.spacing.s3 * ui_scale).abs() < 0.0001);
    assert!((theme.tokens.radii.control - base.tokens.radii.control * ui_scale).abs() < 0.0001);
    assert!((theme.padding_standard - 17.0 * ui_scale).abs() < 0.0001);
    assert!((theme.gap_standard - 7.0 * ui_scale).abs() < 0.0001);
    assert!((theme.radius_standard - 14.0 * f64::from(ui_scale)).abs() < 0.0001);
}

thread_local! {
    static COMMITTED_ACTION: RefCell<Option<Vec<automerge::ChangeHash>>> = const { RefCell::new(None) };
    static COMMIT_DRAINS: Cell<usize> = const { Cell::new(0) };
}
/// The probe's next committed action: the heads captured before its writes.
pub fn report_probe_action(before: Vec<automerge::ChangeHash>) {
    COMMITTED_ACTION.set(Some(before));
}
/// How many committed actions the host has taken from the probe.
pub fn probe_commit_drains() -> usize {
    COMMIT_DRAINS.get()
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
    fn hit() -> tilo_ui::search::SearchResult {
        tilo_ui::search::SearchResult::for_action(
            "Probe",
            "Probe",
            tilo_actions::ActionAddress::new("probe:shared", "edit"),
        )
    }
}
impl tilo_ui::search::SearchProvider for ProbeProvider {
    fn name(&self) -> &'static str {
        self.observe("name");
        "Probe"
    }
    fn action_directory_generation(&self) -> Option<u64> {
        self.observe("generation");
        Some(4)
    }
    fn action_directory_cursor_is_current(&self, _: &tilo_ui::search::SearchCursor) -> bool {
        self.observe("cursor");
        true
    }
    fn search(&self, _: &str) -> Vec<tilo_ui::search::SearchResult> {
        self.observe("search");
        vec![Self::hit()]
    }
    fn search_limited(&self, _: &str, limit: usize) -> Vec<tilo_ui::search::SearchResult> {
        self.observe("limited");
        if limit > 0 { vec![Self::hit()] } else { vec![] }
    }
    fn search_page(
        &self,
        _: &tilo_ui::search::SearchQuery,
        _: Option<&tilo_ui::search::SearchCursor>,
        work: usize,
        limit: usize,
    ) -> tilo_ui::search::SearchPage {
        self.observe("page");
        tilo_ui::search::SearchPage {
            results: if work > 0 && limit > 0 {
                vec![Self::hit()]
            } else {
                vec![]
            },
            next: Some(tilo_ui::search::SearchCursor::default()),
            pending: None,
            inspected: work.min(limit).min(1),
        }
    }
    fn resolve_action_resource(
        &self,
        _: &tilo_actions::ActionAddress,
    ) -> Option<tilo_actions::ResourceSpec> {
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
pub fn permissions() -> SharedPermissions {
    SharedPermissions::new(
        PermissionContext::stranger(&tilo_ui_base::permissions::MemberId::person("probe-author"))
            .at_unix_time(100),
    )
}
/// The author as the workspace's Owner: everything is allowed, so an outside
/// effect runs without asking.
pub fn owner_permissions() -> SharedPermissions {
    SharedPermissions::new(
        PermissionContext::member(
            &tilo_ui_base::permissions::MemberId::person("probe-author"),
            tilo_ui_base::permissions::Role::Owner,
        )
        .at_unix_time(100),
    )
}
/// The author as an Editor of the workspace: may make undoable changes.
pub fn editor_permissions() -> SharedPermissions {
    SharedPermissions::new(
        PermissionContext::member(
            &tilo_ui_base::permissions::MemberId::person("probe-author"),
            tilo_ui_base::permissions::Role::Editor,
        )
        .at_unix_time(100),
    )
}
pub fn author() -> MemberId {
    MemberId::person("probe-author")
}
pub fn provenance() -> tilo_actions::ActionContext {
    tilo_actions::ActionContext::default().with_app(APP, "Probe")
}
pub fn denied_scope() -> PermissionScope {
    PermissionScope::workspace(ActionKind::Undoable)
}
pub fn definition(create: tilo_ui::application::ComponentFactory) -> ApplicationDefinition {
    ApplicationDefinition {
        app_id: APP.into(),
        title: "Probe".into(),
        icon: IconDescriptor::Glyph("P"),
        resource_type: ResourceType::new("probe"),
        required: vec![],
        optional: vec![],
        opens: &[],
        workspace_resource: false,
        opens_before_resource: false,
        create,
    }
}
pub fn probe_definition() -> &'static ApplicationDefinition {
    static DEFINITION: std::sync::OnceLock<ApplicationDefinition> = std::sync::OnceLock::new();
    DEFINITION.get_or_init(|| definition(probe_factory))
}
/// Runner conformance uses this one definition and the existing action factory.
pub fn runner_probe_definition() -> &'static ApplicationDefinition {
    static DEFINITION: std::sync::OnceLock<ApplicationDefinition> = std::sync::OnceLock::new();
    DEFINITION.get_or_init(|| definition(action_probe_factory))
}
pub use tilo_ui::headless::fixture_runner_inputs as runner_inputs;
pub fn runner_permissions() -> SharedPermissions {
    SharedPermissions::new(
        PermissionContext::member(
            &tilo_ui_base::permissions::MemberId::person("probe-author"),
            tilo_ui_base::permissions::Role::Owner,
        )
        .at_unix_time(100),
    )
}
pub struct ProbeHarness {
    pub owners: ResourceOwners,
    pub records: ResourceStore,
    pub views: Rc<InstanceViewStore>,
    /// The workspace's one store in this process's memory: what this process wrote, and what a
    /// restarted one reads back through a new harness over it.
    pub workspace: Rc<MemoryWorkspace>,
    /// The port this process writes through: it answers at once, or holds each write until the
    /// test lets it land ([`ProbeHarness::holding_writes`]).
    documents: Rc<HeldWrites>,
}
impl ProbeHarness {
    pub fn new() -> Self {
        let documents = Rc::new(HeldWrites::default());
        Self::with_documents(documents)
    }

    /// A host whose answer to each write waits for the test: a save stays under way for as long
    /// as a test checks what a pending save does, however slowly a loaded machine runs it.
    pub fn holding_writes() -> Self {
        let documents = Rc::new(HeldWrites::holding());
        Self::with_documents(documents)
    }

    /// A host over the store `workspace`, whose writes apply to it at once.
    pub fn over(workspace: Rc<MemoryWorkspace>) -> Self {
        let documents = Rc::new(HeldWrites::over(workspace));
        Self::with_documents(documents)
    }

    fn with_documents(documents: Rc<HeldWrites>) -> Self {
        CALLS.set(0);
        OPERATIONS.with_borrow_mut(Vec::clear);
        COMMITTED_ACTION.set(None);
        COMMIT_DRAINS.set(0);
        INITIALIZATIONS.set(0);
        HANDLES.with_borrow_mut(Vec::clear);
        PROVIDERS.with_borrow_mut(|providers| *providers = None);
        NATIVE_LIFECYCLE.with_borrow_mut(Vec::clear);
        let workspace = documents.workspace();
        let mut owners = ResourceOwners::writing(documents.clone());
        let records = owners.open(APP, WORKSPACE).unwrap();
        let views = Rc::new(
            InstanceViewStore::new(
                Some(Rc::clone(&documents) as Rc<dyn DocumentPort>),
                APP,
                DEVICE,
                WORKSPACE,
            )
            .unwrap(),
        );
        Self {
            owners,
            records,
            views,
            workspace,
            documents,
        }
    }

    /// Let every write this host holds land, as the workspace would take them.
    pub fn accept_writes(&self) {
        self.documents.accept();
    }

    /// The writes the workspace has not answered yet.
    pub fn held_writes(&self) -> Vec<HeldWrite> {
        self.documents.held()
    }

    /// View `instance` of this host's app, as the next start of the app reads it.
    pub fn saved_view(&self, instance: u64) -> Option<SavedInstanceView> {
        saved_view_where(&self.workspace, APP, DEVICE, WORKSPACE, instance)
    }

    /// What a saved view says: enough to tell one from another and from none.
    pub fn view_state(&self, instance: u64) -> Option<(String, Vec<f64>, Option<Vec<u8>>)> {
        self.saved_view(instance).map(|saved| {
            (
                saved.document_id,
                saved.view.scroll,
                saved.application.map(|application| application.payload),
            )
        })
    }

    /// The record `checkout` names as the workspace holds it: `None` for one it does not hold.
    pub fn stored_record(&self, checkout: &DocumentCheckout) -> Option<Vec<u8>> {
        stored_record_where(&self.workspace, APP, WORKSPACE, checkout)
    }

    /// The body of the record `checkout` names as the workspace holds it now, read back by a
    /// process that did not write it.
    pub fn stored_body(&self, checkout: &DocumentCheckout) -> Option<String> {
        // A store reads only the records it opened; the workspace's own bytes are what another
        // process would open.
        let doc = AutoCommit::load(&self.stored_record(checkout)?).unwrap();
        doc.get(ROOT, "body")
            .unwrap()
            .and_then(|(body, _)| body.into_scalar().ok())
            .and_then(|body| body.into_string().ok())
    }
    pub fn prepare(
        &self,
        definition: &ApplicationDefinition,
        instance: u64,
        target: &LaunchTarget,
    ) -> Result<ProcessContext, LaunchError> {
        self.prepare_with_permissions(definition, instance, target, permissions())
    }
    pub fn prepare_with_permissions(
        &self,
        definition: &ApplicationDefinition,
        instance: u64,
        target: &LaunchTarget,
        permissions: SharedPermissions,
    ) -> Result<ProcessContext, LaunchError> {
        ProcessContext::prepare(
            definition,
            ProcessContextInputs {
                records: self.records.clone(),
                views: self.views.clone(),
                device_id: DEVICE.into(),
                instance_id: instance,
                permissions,
                author: author(),
                action_context: provenance(),
            },
            target,
        )
    }
}
pub fn instantiate_probe(
    host: &ProbeHarness,
    instance: u64,
    target: LaunchTarget,
    restore: RestoreInput,
) -> Result<AppComponent, LaunchError> {
    let context = host.prepare(probe_definition(), instance, &target)?;
    probe_definition().instantiate(context, target, restore)
}
pub fn existing(checkout: &DocumentCheckout) -> LaunchTarget {
    LaunchTarget::Record {
        checkout: checkout.clone(),
        reveal: None,
    }
}
pub fn body(records: &ResourceStore, checkout: &DocumentCheckout) -> String {
    records
        .read(&checkout.id, |doc| {
            doc.get(ROOT, "body")
                .unwrap()
                .unwrap()
                .0
                .as_str()
                .unwrap()
                .to_owned()
        })
        .unwrap()
}
#[allow(
    clippy::panic_in_result_fn,
    reason = "test assertions verify the once-only initializer contract"
)]
fn initialize(context: &mut ProcessContext, target: &LaunchTarget) -> Result<(), LaunchError> {
    HANDLES.with_borrow_mut(|v| v.push(context.wake_handle()));
    if matches!(target, LaunchTarget::New) {
        context
            .initialize_new_resource(|doc| {
                INITIALIZATIONS.set(initializations().saturating_add(1));
                doc.put(ROOT, "body", "initialized")
                    .map_err(|e| e.to_string())?;
                Ok(())
            })
            .map_err(LaunchError::Record)?;
        assert!(
            context
                .initialize_new_resource(|_| panic!("second initializer"))
                .is_err()
        );
    } else {
        assert!(
            context
                .initialize_new_resource(|_| panic!("existing initializer"))
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
    context: ProcessContext,
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
    context: ProcessContext,
    target: LaunchTarget,
    _: RestoreInput,
) -> Result<AppComponent, LaunchError> {
    build_probe(context, &target, true)
}
pub fn operation_observations() -> Vec<Rc<RefCell<Vec<String>>>> {
    OPERATIONS.with_borrow(Clone::clone)
}
fn build_probe(
    mut context: ProcessContext,
    target: &LaunchTarget,
    operational: bool,
) -> Result<AppComponent, LaunchError> {
    CALLS.set(calls().saturating_add(1));
    initialize(&mut context, target)?;
    let body = context
        .read_resource(|doc| {
            doc.get(ROOT, "body")
                .unwrap()
                .unwrap()
                .0
                .as_str()
                .unwrap()
                .to_owned()
        })
        .map_err(LaunchError::Record)?;
    let local = LocalState::new(body);
    local.writer().set("construction wake".into());
    LOCAL.with_borrow_mut(|v| *v = Some(local.clone()));
    let mut registry = tilo_actions::ActionRegistry::new();
    registry.register::<ProbeAction>("probe.edit");
    tilo_ui::history_source::DocumentHistoryEdit::register(&mut registry);
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
            local_snapshot: String::new(),
            scroll: vec![],
            caret: String::new(),
            focused: false,
            corrupt_restore: false,
            rect: vello::kurbo::Rect::ZERO,
        }),
        registry,
    ))
}
#[allow(
    clippy::needless_pass_by_value,
    reason = "ComponentFactory has an owned target signature"
)]
pub fn fail_after_initialization(
    mut context: ProcessContext,
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
    context: ProcessContext,
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
            local_snapshot: String::new(),
            scroll: vec![],
            caret: String::new(),
            focused: false,
            corrupt_restore: false,
            rect: vello::kurbo::Rect::ZERO,
        }),
        tilo_actions::ActionRegistry::new(),
    ))
}
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct ProbeAction(pub String);
impl tilo_actions::Action for ProbeAction {
    fn apply(&self, doc: &mut AutoCommit) -> Result<(), tilo_actions::ActionError> {
        doc.put(ROOT, "body", self.0.clone())?;
        Ok(())
    }
}
struct ProbeRoot {
    operational: bool,
    operations: Rc<RefCell<Vec<String>>>,
    local: LocalState<String>,
    local_snapshot: String,
    scroll: Vec<f64>,
    caret: String,
    focused: bool,
    corrupt_restore: bool,
    rect: vello::kurbo::Rect,
}
impl Widget for ProbeRoot {
    tilo_ui_base::provides!(
        dyn tilo_ui_base::widget::NativeViewLifecycle,
        dyn tilo_ui_base::widget::resources::PublishesResources,
        dyn tilo_ui::search::capability::SearchSource,
        dyn tilo_ui::view::capability::ViewState,
        dyn tilo_ui_base::widget::undo_points::UndoPoints,
        dyn tilo_ui::undo::CommittedActions,
        dyn tilo_ui_base::host_operation::ResourceOperations
    );

    fn set_motion_reduced(&mut self, reduced: bool) {
        PRESENTATION.with_borrow(|slot| {
            if let Some(observations) = slot {
                observations.borrow_mut().motion.push(reduced);
            }
        });
    }

    fn sync(&mut self, _: &AppModel, _: &ModelPath) {
        assert!(self.operational, "construction must project read-only");
        self.local_snapshot = self.local.with(Clone::clone);
    }
    fn build_layout(
        &mut self,
        tree: &mut taffy::TaffyTree<()>,
        theme: &tilo_ui_base::Theme,
        map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        PRESENTATION.with_borrow(|slot| {
            if let Some(observations) = slot {
                observations.borrow_mut().layouts.push(theme.clone());
            }
        });
        let style = taffy::Style {
            size: taffy::Size::from_lengths(80.0, 30.0),
            ..Default::default()
        };
        let node = tree.new_leaf(style).unwrap();
        map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }
    tilo_ui_base::rect_geometry!();
    fn draw(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (_, _, text, theme, mut env) = ctx.parts();
        PRESENTATION.with_borrow(|slot| {
            if let Some(observations) = slot {
                let mut observations = observations.borrow_mut();
                observations.draws.push((theme.clone(), text.text_scale()));
                observations.local_draws.push(self.local_snapshot.clone());
            }
        });
    }
    fn name(&self) -> &'static str {
        "application-probe"
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
}

impl tilo_ui_base::host_operation::ResourceOperations for ProbeRoot {
    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &["probe.view", "probe.effect", "probe.pending"]
    }

    fn start_operation(
        &mut self,
        op: &tilo_ui_base::host_operation::ResourceOperation<'_>,
        _: &mut tilo_ui_base::widget::Edits<'_>,
        _: &ObjId,
    ) -> Result<Option<tilo_ui_base::host_operation::ResourceOperationStart>, String> {
        let tilo_ui_base::host_operation::OperationRequest { kind, type_id, .. } = op.request();
        if !tilo_ui_base::host_operation::local_resource_operation_handlers(self).contains(&type_id) {
            return Ok(None);
        }
        if type_id == "probe.pending" {
            self.operations.borrow_mut().push("pending".into());
            return Ok(Some(
                tilo_ui_base::host_operation::ResourceOperationStart::Accepted,
            ));
        }
        self.operations.borrow_mut().push(format!("{kind:?}"));
        Ok(Some(
            tilo_ui_base::host_operation::ResourceOperationStart::Completed(
                tilo_actions::ActionOutput::default(),
            ),
        ))
    }

    fn poll_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        cancel: bool,
        _: &AutoCommit,
        _: &ObjId,
    ) -> Option<tilo_actions::InvocationUpdate> {
        if !cancel {
            return None;
        }
        assert!(!self.local.writer().try_set("cancelled after close".into()));
        self.operations.borrow_mut().push("cancelled".into());
        Some(tilo_actions::InvocationUpdate::Cancelled {
            invocation,
            message: None,
        })
    }
}

impl tilo_ui::undo::CommittedActions for ProbeRoot {
    fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        let before = COMMITTED_ACTION.take()?;
        COMMIT_DRAINS.set(COMMIT_DRAINS.get().saturating_add(1));
        Some(before)
    }
}

impl tilo_ui_base::widget::undo_points::UndoPoints for ProbeRoot {
    fn get_state(&self) -> Option<String> {
        Some(self.caret.clone())
    }
    fn set_state(&mut self, state: &str) {
        self.caret = state.into();
    }
}

impl tilo_ui::view::capability::ViewState for ProbeRoot {
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

impl tilo_ui::search::capability::SearchSource for ProbeRoot {
    fn search_providers(&self) -> Vec<Box<dyn tilo_ui::search::SearchProvider>> {
        PROVIDERS.with_borrow(|providers| {
            providers.as_ref().map_or_else(Vec::new, |observations| {
                vec![Box::new(ProbeProvider {
                    observations: observations.clone(),
                    local: LocalState::new(false),
                })
                    as Box<dyn tilo_ui::search::SearchProvider>]
            })
        })
    }
}

impl tilo_ui_base::widget::resources::PublishesResources for ProbeRoot {
    #[allow(
        clippy::too_many_lines,
        reason = "one shared resource declares the runner action matrix"
    )]
    fn resources(&self, _: &AutoCommit) -> Vec<tilo_actions::ResourceSpec> {
        PRESENTATION.with_borrow(|slot| {
            if let Some(observations) = slot {
                observations
                    .borrow_mut()
                    .local_resource_reads
                    .push(self.local_snapshot.clone());
            }
        });
        if !self.operational {
            return vec![];
        }
        vec![
            tilo_actions::ResourceSpec::new("probe:shared", "probe", "Shared probe")
                .with_action(
                    tilo_actions::ActionSpec::immediate(
                        "edit",
                        "Edit",
                        "probe.edit",
                        &ProbeAction("edited".into()),
                    )
                    .unwrap()
                    .with_description(self.local_snapshot.clone()),
                )
                .with_action(
                    tilo_actions::ActionSpec::operation(
                        "view",
                        "View",
                        tilo_actions::OperationKind::View,
                        "probe.view",
                        &(),
                    )
                    .unwrap()
                    .with_contract(tilo_actions::ActionContract::view()),
                )
                .with_action(
                    tilo_actions::ActionSpec::operation(
                        "effect",
                        "Effect",
                        tilo_actions::OperationKind::Effect,
                        "probe.effect",
                        &(),
                    )
                    .unwrap()
                    .with_contract(tilo_actions::ActionContract::view()),
                )
                .with_action(
                    tilo_actions::ActionSpec::operation(
                        "pending",
                        "Pending",
                        tilo_actions::OperationKind::Effect,
                        "probe.pending",
                        &(),
                    )
                    .unwrap()
                    .with_contract(tilo_actions::ActionContract {
                        execution: tilo_actions::ExecutionMode::Background { cancellable: true },
                        result: tilo_actions::ResultSpec::Job,
                        ..tilo_actions::ActionContract::view()
                    }),
                )
                .with_action(tilo_actions::ActionSpec::prompted_param(
                    "prompt",
                    "Prompt",
                    tilo_actions::ActionParam::required(
                        "value",
                        tilo_actions::ParamType::Text,
                        "Value",
                    ),
                    "Value",
                    tilo_actions::ExecutionLocus::ClientLocal,
                    |value| {
                        tilo_actions::EncodedAction::encode(
                            "probe.edit",
                            &ProbeAction(value.into()),
                        )
                        .unwrap()
                    },
                ))
                .with_action(tilo_actions::ActionSpec::typed(
                    "typed",
                    "Typed",
                    "Value",
                    tilo_actions::ExecutionLocus::ClientLocal,
                    vec![
                        tilo_actions::ActionParam::required(
                            "value",
                            tilo_actions::ParamType::Text,
                            "Value",
                        ),
                        tilo_actions::ActionParam::required(
                            "enabled",
                            tilo_actions::ParamType::Boolean,
                            "Enabled",
                        ),
                    ],
                    |args| {
                        assert_eq!(args.boolean("enabled"), Some(true));
                        tilo_actions::EncodedAction::encode(
                            "probe.edit",
                            &ProbeAction(args.text("value").unwrap().into()),
                        )
                        .unwrap()
                    },
                ))
                .with_action(
                    tilo_actions::ActionSpec::immediate(
                        "confirm",
                        "Confirm",
                        "probe.edit",
                        &ProbeAction("confirmed".into()),
                    )
                    .unwrap()
                    // A question one Confirm answers: a typed name is only
                    // for an irreversible action, and names what it loses.
                    .with_contract(tilo_actions::ActionContract {
                        confirmation: tilo_actions::ConfirmationPolicy::Confirm,
                        ..tilo_actions::ActionContract::default()
                    }),
                ),
        ]
    }
}

impl NativeViewLifecycle for ProbeRoot {
    fn mount_native_view(&mut self, _: &tilo_ui_base::widget::native_view::NativeViewHost<'_>) {
        NATIVE_LIFECYCLE.with_borrow_mut(|events| {
            events.push(("mount", WakeHandle::new()));
        });
    }

    fn unmount_native_view(&mut self) {
        NATIVE_LIFECYCLE.with_borrow_mut(|events| {
            events.push(("unmount", WakeHandle::new()));
        });
    }

    fn native_view_frame_start(&mut self) {
        NATIVE_LIFECYCLE.with_borrow_mut(|events| {
            events.push(("frame", WakeHandle::new()));
        });
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
}

#[allow(
    clippy::panic_in_result_fn,
    reason = "test assertion verifies a factory cannot mount before validation"
)]
pub fn attempt_premature_mount(
    context: ProcessContext,
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
    mut context: ProcessContext,
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
            local_snapshot: String::new(),
            scroll: vec![f64::INFINITY],
            caret: String::new(),
            focused: false,
            corrupt_restore: false,
            rect: vello::kurbo::Rect::ZERO,
        }),
        tilo_actions::ActionRegistry::new(),
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
    mut context: ProcessContext,
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
            local_snapshot: String::new(),
            scroll: vec![],
            caret: String::new(),
            focused: false,
            corrupt_restore: true,
            rect: vello::kurbo::Rect::ZERO,
        }),
        tilo_actions::ActionRegistry::new(),
    ))
}

pub fn set_foreign_context(context: ProcessContext) {
    FOREIGN_CONTEXT.with_borrow_mut(|slot| *slot = Some(context));
}
pub fn foreign_component(
    mut context: ProcessContext,
    target: LaunchTarget,
    restore: RestoreInput,
) -> Result<AppComponent, LaunchError> {
    initialize(&mut context, &target)?;
    drop(context);
    let foreign = FOREIGN_CONTEXT.with_borrow_mut(Option::take).unwrap();
    probe_factory(foreign, target, restore)
}
pub fn initializer_failure(
    mut context: ProcessContext,
    _: LaunchTarget,
    _: RestoreInput,
) -> Result<AppComponent, LaunchError> {
    HANDLES.with_borrow_mut(|handles| handles.push(context.wake_handle()));
    context
        .initialize_new_resource(|doc| {
            doc.put(ROOT, "body", "unpublished")
                .map_err(|e| e.to_string())?;
            Err("schema initialization refused".into())
        })
        .map_err(LaunchError::Record)?;
    Err(LaunchError::Operation(
        "initializer unexpectedly succeeded".into(),
    ))
}

/// A fixture owner publishes a string through the same typed action loan as an app.
pub fn write_fixture_string(edits: &mut tilo_ui_base::widget::Edits<'_>, key: &str, value: &str) {
    #[derive(Debug, serde::Serialize)]
    struct Write {
        key: String,
        value: String,
    }
    impl tilo_actions::Action for Write {
        fn apply(&self, doc: &mut tilo_actions::Doc) -> Result<(), tilo_actions::ActionError> {
            use automerge::transaction::Transactable;
            doc.put(automerge::ROOT, self.key.as_str(), self.value.as_str())?;
            Ok(())
        }
    }
    tilo_ui::local_dispatch::LocalDispatch::new()
        .apply(
            edits,
            "test.fixture.write",
            &Write {
                key: key.into(),
                value: value.into(),
            },
            tilo_actions::ActionContext::default(),
        )
        .unwrap();
}

pub fn write_fixture_flag(edits: &mut tilo_ui_base::widget::Edits<'_>, key: &str) {
    #[derive(Debug, serde::Serialize)]
    struct Flag {
        key: String,
    }
    impl tilo_actions::Action for Flag {
        fn apply(&self, doc: &mut tilo_actions::Doc) -> Result<(), tilo_actions::ActionError> {
            use automerge::transaction::Transactable;
            doc.put(automerge::ROOT, self.key.as_str(), true)?;
            Ok(())
        }
    }
    tilo_ui::local_dispatch::LocalDispatch::new()
        .apply(
            edits,
            "test.fixture.flag",
            &Flag { key: key.into() },
            tilo_actions::ActionContext::default(),
        )
        .unwrap();
}

/// View `instance` of `app` on `device` in `workspace`, as a process reading the store gets it.
pub fn saved_view_where(
    documents: &Rc<MemoryWorkspace>,
    app: &str,
    device: &str,
    workspace: &str,
    instance: u64,
) -> Option<SavedInstanceView> {
    InstanceViewStore::new(
        Some(Rc::clone(documents) as Rc<dyn DocumentPort>),
        app,
        device,
        workspace,
    )
    .unwrap()
    .load(instance)
    .unwrap()
}

/// The record `checkout` names as the store holds it: `None` for one it does not hold.
pub fn stored_record_where(
    documents: &Rc<MemoryWorkspace>,
    app: &str,
    workspace: &str,
    checkout: &DocumentCheckout,
) -> Option<Vec<u8>> {
    let ask = DocumentAsk {
        id: checkout.id.route().to_owned(),
        since: None,
    };
    match documents
        .read(app, workspace, Some(vec![ask]))
        .unwrap()
        .pop()
        .map(|(_, read)| read)
    {
        Some(DocumentRead::Whole { bytes, .. }) => Some(bytes),
        _ => None,
    }
}

/// A workspace store a process writes through: it answers each write at once and applies it, or
/// holds each one until the test says whether the workspace took it. Holding them lets a test
/// watch a save that is under way: the record reads `Saving` until the answer comes, and the
/// process shows the outcome as the store takes it up.
#[derive(Default)]
pub struct HeldWrites {
    workspace: Rc<MemoryWorkspace>,
    state: Rc<HeldState>,
}

#[derive(Default)]
struct HeldState {
    hold: Cell<bool>,
    next: Cell<u64>,
    pending: RefCell<Vec<HeldWrite>>,
    answers: RefCell<std::collections::BTreeMap<u64, Result<(), String>>>,
}

/// One write a holding store took, and has not answered yet.
#[derive(Clone)]
pub struct HeldWrite {
    ticket: u64,
    pub app: String,
    pub workspace: String,
    pub id: String,
    pub changes: Vec<u8>,
    pub kind: String,
}

impl HeldWrites {
    /// The store itself, whose writes apply at once.
    pub fn over(workspace: Rc<MemoryWorkspace>) -> Self {
        Self {
            workspace,
            state: Rc::default(),
        }
    }

    /// A store that holds every write until the test accepts or refuses it.
    pub fn holding() -> Self {
        let held = Self::default();
        held.state.hold.set(true);
        held
    }

    /// The store whose bytes these are.
    pub fn workspace(&self) -> Rc<MemoryWorkspace> {
        Rc::clone(&self.workspace)
    }

    /// The writes it holds, unanswered.
    pub fn held(&self) -> Vec<HeldWrite> {
        self.state.pending.borrow().clone()
    }

    /// Let every held write land, as the workspace would take them.
    pub fn accept(&self) {
        for write in std::mem::take(&mut *self.state.pending.borrow_mut()) {
            let mut written = self.workspace.write(
                &write.app,
                &write.workspace,
                &write.id,
                &write.kind,
                write.changes,
            );
            let answer = written(std::time::Duration::ZERO).unwrap_or(Ok(()));
            self.state.answers.borrow_mut().insert(write.ticket, answer);
        }
    }

    /// Refuse every held write, as a workspace that could not take them says.
    pub fn refuse(&self, reason: &str) {
        for write in std::mem::take(&mut *self.state.pending.borrow_mut()) {
            self.state
                .answers
                .borrow_mut()
                .insert(write.ticket, Err(reason.to_owned()));
        }
    }
}

impl DocumentPort for HeldWrites {
    fn read(
        &self,
        app: &str,
        workspace: &str,
        reads: Option<Vec<DocumentAsk>>,
    ) -> Result<Vec<(String, DocumentRead)>, String> {
        self.workspace.read(app, workspace, reads)
    }

    fn write(&self, app: &str, workspace: &str, id: &str, kind: &str, changes: Vec<u8>) -> Written {
        if !self.state.hold.get() {
            return self.workspace.write(app, workspace, id, kind, changes);
        }
        let ticket = self.state.next.get();
        self.state.next.set(ticket.wrapping_add(1));
        self.state.pending.borrow_mut().push(HeldWrite {
            ticket,
            app: app.to_owned(),
            workspace: workspace.to_owned(),
            id: id.to_owned(),
            changes: changes.clone(),
            kind: kind.to_owned(),
        });
        let state = Rc::clone(&self.state);
        let inner = Rc::clone(&self.workspace);
        let (app, workspace, id) = (app.to_owned(), workspace.to_owned(), id.to_owned());
        let kind = kind.to_owned();
        Box::new(move |wait: std::time::Duration| {
            let answer = match state.answers.borrow_mut().remove(&ticket) {
                Some(answer) => answer,
                // Nobody waited: the write is still under way. Whoever waits for it (a flush,
                // not a frame's tick) is answered as the workspace would answer.
                None if wait.is_zero() => return None,
                None => Ok(()),
            };
            if answer.is_ok() {
                let mut written = inner.write(&app, &workspace, &id, &kind, changes.clone());
                let _ = written(std::time::Duration::ZERO);
            }
            Some(answer)
        })
    }
}
