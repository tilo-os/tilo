//! The History fixtures its tests share, in the crate or above it: an app's registered History
//! and its preview, the roots a managed record's focus and ownership walk, and the live owner a
//! restored effect is driven through.

#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::wildcard_imports,
    clippy::disallowed_methods,
    reason = "a fixture reads as its tests do: its unwraps are the assertion that it built"
)]

use super::capability::FrameworkHistoryTopology;
use super::*;
#[cfg(test)]
use crate::application::{LaunchTarget, RestoreInput};
#[cfg(test)]
use crate::application_test_support::{
    APP, ProbeHarness, runner_permissions, runner_probe_definition,
};
use crate::{State, headless::Driver, widget::Button, widget::Column, widget::Text};
use automerge::{AutoCommit, ObjId, ROOT, ReadDoc};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use taffy::{NodeId, TaffyTree};
use tilo_ui_base::text_render::TextRenderer;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::widget::ActionEmitter;
use tilo_ui_base::widget::Widget;
use tilo_ui_base::widget::{AppModel, ModelPath};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

static DEFINITION: HistoryDefinition = HistoryDefinition {
    app_id: "dev.tilo.history-fixture",
    scopes: &[HistoryScopeDefinition {
        key: "document",
        title: "Notebook",
        presentation: HistoryPresentation::Domain { create: preview },
    }],
};
pub struct NotebookPreview {
    body: String,
    bounds: Rect,
    view: PreviewView,
}
pub fn preview(input: PreviewInput) -> Result<Box<dyn HistoryPreview>, String> {
    let doc = automerge::AutoCommit::load(input.snapshot.bytes()).map_err(|e| e.to_string())?;
    let body = doc
        .get(ROOT, "body")
        .map_err(|e| e.to_string())?
        .and_then(|(value, _)| value.as_str().map(str::to_owned))
        .unwrap_or_default();
    Ok(Box::new(NotebookPreview {
        body,
        bounds: Rect::ZERO,
        view: input.initial_view,
    }))
}
impl HistoryPreview for NotebookPreview {
    fn layout(&mut self, bounds: Rect, _: &mut TextRenderer, _: &Theme) -> Result<(), String> {
        self.bounds = bounds;
        Ok(())
    }
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
    ) {
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.raised,
            None,
            &self.bounds,
        );
        text.draw_text(
            scene,
            "Notebook",
            24.0,
            theme.text_primary,
            transform * Affine::translate((self.bounds.x0 + 24.0, self.bounds.y0 + 24.0)),
            None,
        );
        text.draw_text(
            scene,
            &self.body,
            18.0,
            theme.text_primary,
            transform
                * Affine::translate((
                    self.bounds.x0 + 24.0,
                    self.bounds.y0 + 70.0 + self.view.scroll.1,
                )),
            None,
        );
    }
    fn navigate(&mut self, command: PreviewNavigation) -> bool {
        if let PreviewNavigation::Scroll { dx, dy } = command {
            self.view.scroll.0 += dx;
            self.view.scroll.1 += dy;
            true
        } else {
            false
        }
    }
    fn view(&self) -> PreviewView {
        self.view.clone()
    }
    fn accessibility(&self) -> PreviewAccessibility {
        PreviewAccessibility {
            description: format!("Notebook: {}; scroll {}", self.body, self.view.scroll.1),
            pages: vec![],
        }
    }
}
pub fn registration() -> HistoryRegistration {
    HistoryRegistration::new(
        &DEFINITION,
        vec![HistorySourceRegistration::HostDocument {
            scope: "document",
            resource: HistoryResource::new(
                "fixture",
                "lifetime-1",
                DEFINITION.app_id,
                crate::DocumentId::for_tab(777),
            ),
        }],
    )
    .unwrap()
}

/// Real History composition with a probe at its lifecycle enumeration boundary.
#[tilo_ui_base::component::not_a_component(framework, "a focus fixture the History tests mount")]
pub struct FocusHistoryRoot {
    pub history: HistoryHost,
    pub live: Column,
    visits: Rc<Cell<usize>>,
}

impl FocusHistoryRoot {
    pub fn new(rows: usize, state: &crate::State) -> (Self, Rc<Cell<usize>>) {
        let mut live = Column::new();
        for row in 0..rows {
            live = live.push(Box::new(Button::new(&format!("row-{row}"), "Row")));
        }
        let visits = Rc::new(Cell::new(0));
        let history = HistoryHost::new(registration(), &state.doc).unwrap();
        (
            Self {
                history,
                live,
                visits: visits.clone(),
            },
            visits,
        )
    }
}

impl FrameworkHistoryTopology for FocusHistoryRoot {
    fn visit_framework_history(
        &self,
        visitor: &mut dyn FnMut(&FrameworkHistorySurface, &dyn Widget),
    ) {
        self.visits.set(self.visits.get() + 1);
        visitor(&self.history, &self.live);
        super::capability::visit_framework_history(&self.live, visitor);
    }

    fn visit_framework_history_mut(
        &mut self,
        visitor: &mut dyn FnMut(&mut FrameworkHistorySurface, &mut dyn Widget),
    ) {
        visitor(&mut self.history, &mut self.live);
        super::capability::visit_framework_history_mut(&mut self.live, visitor);
    }
}

impl Widget for FocusHistoryRoot {
    tilo_ui_base::provides!(dyn FrameworkHistoryTopology);
    crate::testing::inert_widget_methods!(name = "History focus probe");

    fn build_layout(
        &mut self,
        tree: &mut TaffyTree<()>,
        theme: &tilo_ui_base::Theme,
        nodes: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.history
            .build_composition(&mut self.live, tree, theme, nodes)
    }

    fn apply_layout(&mut self, tree: &TaffyTree<()>, root: NodeId, origin: vello::kurbo::Point) {
        self.history
            .apply_composition(&mut self.live, tree, root, origin);
    }

    fn get_rect(&self) -> vello::kurbo::Rect {
        self.history.get_rect()
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![&self.history, &self.live]
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![&mut self.history, &mut self.live]
    }
}

pub fn resource() -> tilo_actions::ResourceSpec {
    resource_for("live", "fixture.export")
}
/// The continuation identity both fixture operations declare: this fixture's keys over the resource and
/// the operation.
pub fn fixture_identity(
    target: String,
    handler: String,
) -> impl for<'a> Fn(tilo_actions::ContinuationQuery<'a>) -> Option<tilo_actions::ContinuationEvidence>
{
    let key = move |namespace: &'static str, part: &str| {
        tilo_actions::ContinuationKey::new(namespace, [part.as_bytes()])
    };
    move |query| {
        let handler_key = || key("ui.history.fixture.handler", &handler);
        Some(match query {
            tilo_actions::ContinuationQuery::Exact(_) => {
                tilo_actions::ContinuationEvidence::Exact {
                    target: key("ui.history.fixture.target", &target),
                    handler: handler_key(),
                }
            }
            tilo_actions::ContinuationQuery::Draft => tilo_actions::ContinuationEvidence::Draft {
                resolution_domain: key("ui.history.fixture.domain", &target),
                handler: handler_key(),
            },
        })
    }
}
pub fn resource_for(id: &str, operation: &str) -> tilo_actions::ResourceSpec {
    tilo_actions::ResourceSpec::new(id, "fixture", "Live owner").with_action(
        tilo_actions::ActionSpec::operation(
            "export",
            "Export",
            tilo_actions::OperationKind::Effect,
            operation,
            &(),
        )
        .unwrap()
        .execution(tilo_actions::ExecutionMode::Background { cancellable: false })
        .returns(tilo_actions::ResultSpec::Job)
        .with_continuation_identity(fixture_identity(id.to_owned(), operation.to_owned())),
    )
}
pub fn prompted_resource_for(
    id: &str,
    operation: &str,
    confirmation: tilo_actions::ConfirmationPolicy,
) -> tilo_actions::ResourceSpec {
    let mut spec = tilo_actions::ActionSpec::prompted_operation(
        "export",
        "Export",
        "Payload",
        "payload",
        tilo_actions::OperationKind::Effect,
        operation,
        {
            let operation = operation.to_owned();
            move |value| tilo_actions::EncodedAction::encode_operation(&operation, &value).unwrap()
        },
    );
    spec.type_mut().contract.confirmation = confirmation;
    let spec = spec
        .execution(tilo_actions::ExecutionMode::Background { cancellable: false })
        .returns(tilo_actions::ResultSpec::Job)
        .with_continuation_identity(fixture_identity(id.to_owned(), operation.to_owned()));
    tilo_actions::ResourceSpec::new(id, "fixture", "Live owner").with_action(spec)
}

pub struct MutableContinuationIdentity {
    pub target: Cell<u64>,
    pub handler: Cell<u64>,
    pub supported: Cell<bool>,
    pub duplicate_resource: Cell<bool>,
    pub housekeeping: Cell<bool>,
}

pub fn mutable_identity_resources(
    id: &str,
    operation: &str,
    identity: &MutableContinuationIdentity,
) -> Vec<tilo_actions::ResourceSpec> {
    let target = identity.target.get();
    let handler = identity.handler.get();
    let supported = identity.supported.get();
    let mut spec = tilo_actions::ActionSpec::operation(
        "export",
        "Export",
        tilo_actions::OperationKind::Effect,
        operation,
        &(),
    )
    .unwrap()
    .execution(tilo_actions::ExecutionMode::Background { cancellable: false })
    .returns(tilo_actions::ResultSpec::Job)
    .with_continuation_identity(move |query| {
        supported.then(|| match query {
            tilo_actions::ContinuationQuery::Exact(_) => {
                tilo_actions::ContinuationEvidence::Exact {
                    target: tilo_actions::ContinuationKey::new(
                        "ui.history.mutable.target",
                        [target.to_be_bytes()],
                    ),
                    handler: tilo_actions::ContinuationKey::new(
                        "ui.history.mutable.handler",
                        [handler.to_be_bytes()],
                    ),
                }
            }
            tilo_actions::ContinuationQuery::Draft => tilo_actions::ContinuationEvidence::Draft {
                resolution_domain: tilo_actions::ContinuationKey::new(
                    "ui.history.mutable.domain",
                    [target.to_be_bytes()],
                ),
                handler: tilo_actions::ContinuationKey::new(
                    "ui.history.mutable.handler",
                    [handler.to_be_bytes()],
                ),
            },
        })
    });
    if identity.housekeeping.get() {
        spec.type_mut()
            .contract
            .tags
            .push(tilo_actions::HOUSEKEEPING_TAG.to_owned());
    }
    let resource = tilo_actions::ResourceSpec::new(id, "fixture", "Live owner").with_action(spec);
    if identity.duplicate_resource.get() {
        vec![
            resource,
            tilo_actions::ResourceSpec::new(id, "fixture", "Duplicate declaration"),
        ]
    } else {
        vec![resource]
    }
}

#[tilo_ui_base::component::not_a_component(
    framework,
    "a live owner the History tests drive effects through"
)]
pub struct LiveOwner {
    pub resource_id: &'static str,
    pub type_id: &'static str,
    pub text: Text,
    pub emitter: Rc<RefCell<ActionEmitter<()>>>,
    pub starts: Rc<Cell<usize>>,
    pub finish: Rc<Cell<bool>>,
    pub completions: Rc<Cell<usize>>,
    pub local_events: Rc<Cell<usize>>,
    pub collab_events: Rc<Cell<usize>>,
    pub collab_wake_handles: Rc<RefCell<Vec<crate::WakeHandle>>>,
    pub bracket: Rc<RefCell<Option<Vec<automerge::ChangeHash>>>>,
    pub undo_request: Rc<Cell<Option<tilo_ui_base::widget::undo_access::UndoRequest>>>,
    pub active: Option<tilo_actions::InvocationId>,
    pub manual: Rc<RefCell<Option<tilo_actions::ControlInvocation>>>,
    pub confirmation: tilo_actions::ConfirmationPolicy,
    pub prompted: bool,
    pub mutable_identity: Option<Rc<MutableContinuationIdentity>>,
}
impl tilo_ui_base::widget::Collaborative for LiveOwner {
    fn handle_collab_event(&mut self, _: &tilo_ui_base::collaboration::CollabEvent) {
        self.collab_events.set(self.collab_events.get() + 1);
        self.collab_wake_handles
            .borrow_mut()
            .push(crate::WakeHandle::new());
    }
}

impl Widget for LiveOwner {
    tilo_ui_base::provides!(
        dyn tilo_ui_base::widget::Collaborative,
        dyn tilo_ui_base::widget::undo_access::CommittedActions,
        dyn tilo_ui_base::widget::undo_access::UndoControl,
        dyn tilo_ui_base::widget::undo_access::LocalUndo,
        dyn tilo_ui_base::widget::controls::ActionControls,
        dyn crate::widget::resources::PublishesResources,
        dyn tilo_ui_base::widget::ResourceOperations
    );

    fn name(&self) -> &str {
        "Live owner"
    }
    fn test_id(&self) -> Option<&str> {
        Some(if self.resource_id == "live" {
            "live-owner"
        } else {
            self.resource_id
        })
    }
    fn sync(&mut self, doc: &AppModel, path: &ModelPath) {
        self.text.sync(doc, path);
    }
    fn build_layout(
        &mut self,
        tree: &mut TaffyTree<()>,
        theme: &Theme,
        nodes: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = self.text.build_layout(tree, theme, nodes);
        nodes.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }
    fn apply_layout(&mut self, tree: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.text.apply_layout(tree, node, origin);
    }
    fn draw(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text, theme, mut env) = ctx.parts();
        self.text.draw(&mut tilo_ui_base::draw::DrawCtx::new(
            scene,
            transform,
            text,
            theme,
            env.clone(),
        ));
    }
    fn get_rect(&self) -> Rect {
        self.text.get_rect()
    }
    fn handle_event(&mut self, _: &WindowEvent, _ctx: &mut tilo_ui_base::widget::EventCtx<'_>) {
        self.local_events.set(self.local_events.get() + 1);
    }
}

impl tilo_ui_base::widget::ResourceOperations for LiveOwner {
    fn resource_operation_handlers(&self) -> std::collections::HashSet<String> {
        [self.type_id.to_owned()].into_iter().collect()
    }

    fn start_operation(
        &mut self,
        op: &tilo_ui_base::widget::ResourceOperation<'_>,
        _: &mut tilo_ui_base::widget::Edits<'_>,
        _: &ObjId,
    ) -> Result<Option<tilo_ui_base::widget::ResourceOperationStart>, String> {
        let tilo_ui_base::host_operation::OperationRequest {
            invocation,
            type_id,
            ..
        } = op.request();
        if type_id != self.type_id {
            return Ok(None);
        }
        self.starts.set(self.starts.get() + 1);
        self.active = Some(invocation);
        Ok(Some(tilo_ui_base::widget::ResourceOperationStart::Accepted))
    }

    fn poll_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        _: bool,
        _: &AutoCommit,
        _: &ObjId,
    ) -> Option<tilo_actions::InvocationUpdate> {
        if self.active != Some(invocation) || !self.finish.get() {
            return None;
        }
        self.active = None;
        self.completions.set(self.completions.get() + 1);
        Some(tilo_actions::InvocationUpdate::Succeeded {
            invocation,
            output: tilo_actions::ActionOutput::default(),
        })
    }
}

impl crate::widget::resources::PublishesResources for LiveOwner {
    fn resources(&self, _: &AutoCommit) -> Vec<tilo_actions::ResourceSpec> {
        if let Some(identity) = &self.mutable_identity {
            return mutable_identity_resources(self.resource_id, self.type_id, identity);
        }
        vec![if self.prompted {
            prompted_resource_for(self.resource_id, self.type_id, self.confirmation.clone())
        } else {
            let mut resource = resource_for(self.resource_id, self.type_id);
            resource.actions[0].type_mut().contract.confirmation = self.confirmation.clone();
            resource
        }]
    }
}

impl tilo_ui_base::widget::controls::ActionControls for LiveOwner {
    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        self.manual
            .borrow_mut()
            .take()
            .or_else(|| self.emitter.borrow_mut().take())
    }
}

impl tilo_ui_base::widget::undo_access::LocalUndo for LiveOwner {
    fn handle_local_undo_request(
        &mut self,
        _: tilo_ui_base::widget::undo_access::UndoRequest,
    ) -> bool {
        self.local_events.set(self.local_events.get() + 1);
        true
    }
}

impl tilo_ui_base::widget::undo_access::UndoControl for LiveOwner {
    fn take_undo_request(&mut self) -> Option<tilo_ui_base::widget::undo_access::UndoRequest> {
        self.undo_request.take()
    }
}

impl tilo_ui_base::widget::undo_access::CommittedActions for LiveOwner {
    fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.bracket.borrow_mut().take()
    }
}
pub struct EffectProbe {
    pub root: Box<dyn crate::widget::AppWidget>,
    pub emitter: Rc<RefCell<ActionEmitter<()>>>,
    pub starts: Rc<Cell<usize>>,
    pub finish: Rc<Cell<bool>>,
    pub completions: Rc<Cell<usize>>,
    pub collab_events: Rc<Cell<usize>>,
    pub collab_wake_handles: Rc<RefCell<Vec<crate::WakeHandle>>>,
}
pub fn native_effect_probe_parts(id: &'static str, operation: &'static str) -> EffectProbe {
    let starts = Rc::new(Cell::new(0));
    let finish = Rc::new(Cell::new(false));
    let completions = Rc::new(Cell::new(0));
    let emitter = Rc::new(RefCell::new(ActionEmitter::new(crate::Bind::bound(
        tilo_actions::ActionBinding::unit_for(&resource_for(id, operation), "export").unwrap(),
    ))));
    let collab_events = Rc::new(Cell::new(0));
    let collab_wake_handles = Rc::new(RefCell::new(Vec::new()));
    let root = Box::new(LiveOwner {
        resource_id: id,
        type_id: operation,
        bracket: Rc::new(RefCell::new(None)),
        undo_request: Rc::new(Cell::new(None)),
        text: Text::new(id),
        emitter: emitter.clone(),
        starts: starts.clone(),
        finish: finish.clone(),
        completions: completions.clone(),
        collab_events: collab_events.clone(),
        collab_wake_handles: collab_wake_handles.clone(),
        local_events: Rc::new(Cell::new(0)),
        active: None,
        manual: Rc::new(RefCell::new(None)),
        confirmation: tilo_actions::ConfirmationPolicy::None,
        prompted: false,
        mutable_identity: None,
    });
    EffectProbe {
        root,
        emitter,
        starts,
        finish,
        completions,
        collab_events,
        collab_wake_handles,
    }
}
impl crate::widget::AppWidget for LiveOwner {
    fn app_kind(&self) -> crate::widget::AppKind {
        crate::widget::AppKind::Editor
    }
    fn app_title(&self) -> &str {
        "Live effect probe"
    }
    fn app_icon(&self) -> crate::widget::IconDescriptor {
        crate::widget::IconDescriptor::Glyph("P")
    }
}

pub fn retained_identity_fixture(
    identity: Rc<MutableContinuationIdentity>,
) -> (Driver, Rc<Cell<usize>>, Rc<RefCell<ActionEmitter<()>>>) {
    let starts = Rc::new(Cell::new(0));
    let resource =
        mutable_identity_resources("mutable-owner", "fixture.mutable-export", &identity).remove(0);
    let emitter = Rc::new(RefCell::new(ActionEmitter::new(crate::Bind::bound(
        tilo_actions::ActionBinding::unit_for(&resource, "export").unwrap(),
    ))));
    let owner = LiveOwner {
        resource_id: "mutable-owner",
        type_id: "fixture.mutable-export",
        text: Text::new("Mutable owner"),
        emitter: emitter.clone(),
        starts: starts.clone(),
        finish: Rc::new(Cell::new(false)),
        completions: Rc::new(Cell::new(0)),
        local_events: Rc::new(Cell::new(0)),
        collab_events: Rc::new(Cell::new(0)),
        collab_wake_handles: Rc::new(RefCell::new(Vec::new())),
        bracket: Rc::new(RefCell::new(None)),
        undo_request: Rc::new(Cell::new(None)),
        active: None,
        manual: Rc::new(RefCell::new(Some(tilo_actions::ControlInvocation {
            address: tilo_actions::ActionAddress::new("mutable-owner", "export"),
            arguments: serde_json::Map::new(),
        }))),
        confirmation: tilo_actions::ConfirmationPolicy::None,
        prompted: false,
        mutable_identity: Some(identity),
    };
    (
        Driver::from_managed_root(
            State::new(),
            ManagedRoot::new(Box::new(owner), registration()),
            (900, 650),
        )
        .unwrap(),
        starts,
        emitter,
    )
}

#[cfg(test)]
static BOUND: HistoryDefinition = HistoryDefinition {
    app_id: APP,
    scopes: &[HistoryScopeDefinition {
        key: "document",
        title: "Bound notebook",
        presentation: HistoryPresentation::Domain { create: preview },
    }],
};

/// The text a restore fixture's record holds at `body`.
pub fn body(doc: &automerge::AutoCommit) -> String {
    use automerge::ReadDoc as _;
    doc.get(ROOT, "body")
        .unwrap()
        .unwrap()
        .0
        .as_str()
        .unwrap()
        .to_owned()
}

pub static RESTORABLE: HistoryDefinition = HistoryDefinition {
    app_id: "dev.tilo.restore-fixture",
    scopes: &[HistoryScopeDefinition {
        key: "document",
        title: "Notebook",
        presentation: HistoryPresentation::Domain { create: preview },
    }],
};

#[cfg(test)]
pub fn component_with_history() -> (ProbeHarness, crate::application::AppComponent) {
    let host = ProbeHarness::new();
    let definition = runner_probe_definition();
    let context = host
        .prepare_with_permissions(definition, 950, &LaunchTarget::New, runner_permissions())
        .unwrap();
    let mut component = definition
        .instantiate(context, LaunchTarget::New, RestoreInput::None)
        .unwrap();
    component.register_document_history(&BOUND).unwrap();
    (host, component)
}
