//! Test-only application definition. Every persistence and authority input is explicit.
use automerge::{AutoCommit, ObjId, ROOT, ReadDoc, transaction::Transactable};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use tilo_ui::TextRole;
use tilo_ui::application::{
    AppComponent, ApplicationDefinition, HostContext, HostContextInputs, LaunchTarget, RestoreInput,
};
use tilo_ui::application_view::InstanceViewStore;
use tilo_ui::launch::LaunchError;
use tilo_ui::local_state::LocalState;
use tilo_ui::permissions::{ActionKind, PermissionContext, PermissionScope, UserId};
use tilo_ui::record::{RecordCheckout, RecordKind, RecordOwners, RecordStore};
use tilo_ui::view::ViewContribution;
use tilo_ui::wake::WakeHandle;
use tilo_ui::widget::{AppKind, AppWidget, IconDescriptor, NativeViewLifecycle, Widget};
use tilo_ui::widget::{AppModel, ModelPath};

pub const APP: &str = "so.tilo.application-probe";
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
    static PRESENTATION: RefCell<Option<Rc<RefCell<PresentationObservations>>>> = const { RefCell::new(None) };
    static NATIVE_LIFECYCLE: RefCell<Vec<(&'static str, WakeHandle)>> = const { RefCell::new(vec![]) };
}

pub fn take_native_lifecycle_observations() -> Vec<(&'static str, WakeHandle)> {
    NATIVE_LIFECYCLE.with_borrow_mut(std::mem::take)
}

#[derive(Default)]
pub struct PresentationObservations {
    pub layouts: Vec<tilo_ui::Theme>,
    pub draws: Vec<(tilo_ui::Theme, f32)>,
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
pub fn assert_presentation(theme: &tilo_ui::Theme, scale: f32, ui_scale: f32) {
    assert_eq!(theme.text_scale, scale);
    assert_eq!(theme.ui_scale, ui_scale);
    assert_eq!(theme.name, "explicit presentation");
    assert!(theme.reduce_motion);
    assert_eq!(theme.tokens.colors.intent.accent, theme.status_success);
    assert!(
        (theme.tokens.typography.size(TextRole::Body) - 19.0 * ui_scale * scale).abs() < 0.0001
    );
    let base = tilo_ui::Theme::dark();
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
    ) -> Option<tilo_actions::Resource> {
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
        PermissionContext::stranger("probe-author").at_unix_time(100),
    ))
}
/// The author as an Editor of the room: may make undoable changes.
pub fn editor_permissions() -> Rc<RefCell<PermissionContext>> {
    Rc::new(RefCell::new(
        PermissionContext::member("probe-author", tilo_ui::permissions::Role::Editor)
            .at_unix_time(100),
    ))
}
pub fn author() -> UserId {
    UserId("probe-author".into())
}
pub fn provenance() -> tilo_actions::ActionContext {
    tilo_actions::ActionContext::default().with_app(APP, "Probe")
}
pub fn denied_scope() -> PermissionScope {
    PermissionScope::workspace(ActionKind::Undoable)
}
pub fn definition(create: tilo_ui::application::ComponentFactory) -> ApplicationDefinition {
    ApplicationDefinition {
        app_id: APP,
        title: "Probe",
        icon: IconDescriptor::Glyph("P"),
        record_kind: RecordKind::new("probe"),
        required: vec![],
        optional: vec![],
        opens: &[],
        room_record: false,
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
pub fn runner_inputs() -> tilo_ui::application::ComponentRunnerInputs {
    let catalog = tilo_ui::font_catalog::FontCatalog::controlled_fixture(
        tilo_ui::font_catalog::checkout_root(),
        "en-US",
    );
    tilo_ui::application::ComponentRunnerInputs {
        theme: tilo_ui::Theme::dark(),
        text_renderer: tilo_ui::TextRenderer::with_font_catalog(&catalog).unwrap(),
    }
}
pub fn runner_permissions() -> Rc<RefCell<PermissionContext>> {
    Rc::new(RefCell::new(
        PermissionContext::member("probe-author", tilo_ui::permissions::Role::Owner)
            .at_unix_time(100),
    ))
}
pub struct ProbeHost {
    pub owners: RecordOwners,
    pub records: RecordStore,
    pub views: Rc<InstanceViewStore>,
    // Keep the filesystem alive until stores flush and join their owned workers.
    pub temp: tempfile::TempDir,
}
impl ProbeHost {
    pub fn new() -> Self {
        CALLS.set(0);
        OPERATIONS.with_borrow_mut(Vec::clear);
        COMMITTED_ACTION.set(None);
        COMMIT_DRAINS.set(0);
        INITIALIZATIONS.set(0);
        HANDLES.with_borrow_mut(Vec::clear);
        PROVIDERS.with_borrow_mut(|providers| *providers = None);
        NATIVE_LIFECYCLE.with_borrow_mut(Vec::clear);
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("state");
        let mut owners = RecordOwners::new(root.clone());
        let records = owners.open(APP, ROOM).unwrap();
        let views = Rc::new(InstanceViewStore::new(root, DEVICE, ROOM).unwrap());
        Self {
            owners,
            records,
            views,
            temp,
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
            HostContextInputs {
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
    tilo_ui::provides!(
        dyn tilo_ui::widget::NativeViewLifecycle,
        dyn tilo_ui::resources::PublishesResources,
        dyn tilo_ui::search::capability::SearchSource,
        dyn tilo_ui::view::capability::ViewState,
        dyn tilo_ui::undo::UndoPoints,
        dyn tilo_ui::undo::CommittedActions
    );

    fn set_motion_reduced(&mut self, reduced: bool) {
        PRESENTATION.with_borrow(|slot| {
            if let Some(observations) = slot {
                observations.borrow_mut().motion.push(reduced);
            }
        });
    }

    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &["probe.view", "probe.effect", "probe.pending"]
    }
    fn start_operation(
        &mut self,
        op: &tilo_ui::host_operation::ResourceOperation<'_>,
        _: &mut tilo_ui::widget::Edits<'_>,
        _: &ObjId,
    ) -> Result<Option<tilo_ui::host_operation::ResourceOperationStart>, String> {
        let Some(tilo_ui::host_operation::OperationRequest { kind, type_id, .. }) = op.unscoped()
        else {
            return Ok(None);
        };
        if !self.local_resource_operation_handlers().contains(&type_id) {
            return Ok(None);
        }
        if type_id == "probe.pending" {
            self.operations.borrow_mut().push("pending".into());
            return Ok(Some(
                tilo_ui::host_operation::ResourceOperationStart::Accepted,
            ));
        }
        self.operations.borrow_mut().push(format!("{kind:?}"));
        Ok(Some(
            tilo_ui::host_operation::ResourceOperationStart::Completed(
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
    fn sync(&mut self, _: &AppModel, _: &ModelPath) {
        assert!(self.operational, "construction must project read-only");
        self.local_snapshot = self.local.with(Clone::clone);
    }
    fn build_layout(
        &mut self,
        tree: &mut taffy::TaffyTree<()>,
        theme: &tilo_ui::Theme,
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
    tilo_ui::rect_geometry!();
    fn draw(
        &mut self,
        _: &mut vello::Scene,
        _: vello::kurbo::Affine,
        text: &mut tilo_ui::TextRenderer,
        theme: &tilo_ui::Theme,
        _: &tilo_ui::PresenceCtx,
    ) {
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

impl tilo_ui::undo::CommittedActions for ProbeRoot {
    fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        let before = COMMITTED_ACTION.take()?;
        COMMIT_DRAINS.set(COMMIT_DRAINS.get().saturating_add(1));
        Some(before)
    }
}

impl tilo_ui::undo::UndoPoints for ProbeRoot {
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

impl tilo_ui::resources::PublishesResources for ProbeRoot {
    #[allow(
        clippy::too_many_lines,
        reason = "one shared resource declares the runner action matrix"
    )]
    fn resources(&self, _: &AutoCommit) -> Vec<tilo_actions::Resource> {
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
            tilo_actions::Resource::new("probe:shared", "probe", "Shared probe")
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
                    .with_contract(tilo_actions::ActionContract {
                        confirmation: tilo_actions::ConfirmationPolicy::Phrase {
                            phrase: "JA".into(),
                        },
                        ..tilo_actions::ActionContract::default()
                    }),
                ),
        ]
    }
}

impl NativeViewLifecycle for ProbeRoot {
    fn mount_native_view(&mut self, _: &tilo_ui::widget::native_view::NativeViewHost<'_>) {
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

/// A fixture owner publishes a string through the same typed action loan as an app.
pub fn write_fixture_string(edits: &mut tilo_ui::widget::Edits<'_>, key: &str, value: &str) {
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
    edits
        .apply(
            &mut tilo_ui::action_host::ActionHost::new(),
            "test.fixture.write",
            &Write {
                key: key.into(),
                value: value.into(),
            },
            tilo_actions::ActionContext::default(),
        )
        .unwrap();
}

pub fn write_fixture_flag(edits: &mut tilo_ui::widget::Edits<'_>, key: &str) {
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
    edits
        .apply(
            &mut tilo_ui::action_host::ActionHost::new(),
            "test.fixture.flag",
            &Flag { key: key.into() },
            tilo_actions::ActionContext::default(),
        )
        .unwrap();
}
