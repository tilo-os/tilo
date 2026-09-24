//! Real retained controls and owner operation polling exercise the host boundary.
use super::host_tests::registration;
use super::*;
use crate::{
    State,
    headless::Driver,
    widget::{ActionEmitter, Text, Widget},
};
use crate::{presence::PresenceCtx, text_render::TextRenderer, theme::Theme};
use automerge::{AutoCommit, ObjId, ROOT, transaction::Transactable};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    rc::Rc,
};
use taffy::{NodeId, TaffyTree};
use vello::{
    Scene,
    kurbo::{Affine, Point, Rect},
};
use winit::event::WindowEvent;

fn resource() -> tilo_actions::Resource {
    resource_for("live", "fixture.export")
}
fn resource_for(id: &str, operation: &str) -> tilo_actions::Resource {
    tilo_actions::Resource::new(id, "fixture", "Live owner").with_action(
        tilo_actions::ActionSpec::operation(
            "export",
            "Export",
            tilo_actions::OperationKind::Effect,
            operation,
            &(),
        )
        .unwrap()
        .execution(tilo_actions::ExecutionMode::Background { cancellable: false })
        .returns(tilo_actions::ResultSpec::Job),
    )
}
struct LiveOwner {
    resource_id: &'static str,
    type_id: &'static str,
    text: Text,
    emitter: Rc<RefCell<ActionEmitter<()>>>,
    starts: Rc<Cell<usize>>,
    finish: Rc<Cell<bool>>,
    completions: Rc<Cell<usize>>,
    local_events: Rc<Cell<usize>>,
    collab_events: Rc<Cell<usize>>,
    bracket: Rc<RefCell<Option<Vec<automerge::ChangeHash>>>>,
    undo_request: Rc<Cell<Option<crate::undo::UndoRequest>>>,
    active: Option<tilo_actions::InvocationId>,
}

impl crate::widget::Collaborative for LiveOwner {
    fn handle_collab_event(&mut self, _: &crate::collaboration::CollabEvent) {
        self.collab_events.set(self.collab_events.get() + 1);
    }
}

impl Widget for LiveOwner {
    fn collaborative_mut(&mut self) -> Option<&mut dyn crate::widget::Collaborative> {
        Some(self)
    }

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
    fn sync(&mut self, doc: &mut AutoCommit, path: &ObjId) {
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
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.text.draw(scene, transform, text, theme, presence);
    }
    fn get_rect(&self) -> Rect {
        self.text.get_rect()
    }
    fn handle_event(
        &mut self,
        _: &WindowEvent,
        _: f64,
        _: &winit::event::Modifiers,
        _: &mut AutoCommit,
        _: &ObjId,
        _: &mut bool,
        _: &mut Option<NodeId>,
        _: NodeId,
        _: &TaffyTree<()>,
    ) {
        self.local_events.set(self.local_events.get() + 1);
    }
    fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.bracket.borrow_mut().take()
    }
    fn take_undo_request(&mut self) -> Option<crate::undo::UndoRequest> {
        self.undo_request.take()
    }
    fn handle_local_undo_request(&mut self, _: crate::undo::UndoRequest) -> bool {
        self.local_events.set(self.local_events.get() + 1);
        true
    }
    fn resources(&self, _: &AutoCommit) -> Vec<tilo_actions::Resource> {
        vec![resource_for(self.resource_id, self.type_id)]
    }
    fn resource_operation_handlers(&self) -> std::collections::HashSet<String> {
        [self.type_id.to_owned()].into_iter().collect()
    }
    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        self.emitter.borrow_mut().take()
    }
    fn start_resource_operation_with_document(
        &mut self,
        invocation: tilo_actions::InvocationId,
        _: tilo_actions::OperationKind,
        type_id: &str,
        _: &[u8],
        _: &mut AutoCommit,
        _: &ObjId,
    ) -> Result<Option<crate::widget::ResourceOperationStart>, String> {
        if type_id != self.type_id {
            return Ok(None);
        }
        self.starts.set(self.starts.get() + 1);
        self.active = Some(invocation);
        Ok(Some(crate::widget::ResourceOperationStart::Accepted))
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
#[test]
fn preview_discards_unadmitted_local_intents_but_polls_an_already_admitted_effect() {
    let emitter = Rc::new(RefCell::new(ActionEmitter::new(
        tilo_actions::ActionBinding::unit_for(&resource(), "export").unwrap(),
    )));
    let starts = Rc::new(Cell::new(0));
    let finish = Rc::new(Cell::new(false));
    let completions = Rc::new(Cell::new(0));
    let local_events = Rc::new(Cell::new(0));
    let owner = LiveOwner {
        resource_id: "live",
        type_id: "fixture.export",
        text: Text::new("Live owner"),
        emitter: emitter.clone(),
        starts: starts.clone(),
        finish: finish.clone(),
        completions: completions.clone(),
        local_events: local_events.clone(),
        collab_events: Rc::new(Cell::new(0)),
        bracket: Rc::new(RefCell::new(None)),
        undo_request: Rc::new(Cell::new(None)),
        active: None,
    };
    let mut state = State::new();
    state.doc.put(ROOT, "body", "first").unwrap();
    state.commit_action(None, None);
    let mut driver = Driver::from_managed_root(
        state,
        ManagedRoot::new(Box::new(owner), registration()),
        (900, 650),
    )
    .unwrap();
    emitter.borrow_mut().emit(()).unwrap();
    driver.key("x");
    assert_eq!(
        starts.get(),
        1,
        "real local host door admitted the first effect"
    );
    emitter.borrow_mut().emit(()).unwrap();
    driver.click_named("history-open").unwrap();
    assert!(
        emitter.borrow_mut().take().is_none(),
        "opening History invalidates queued local intent"
    );
    driver
        .perform_named("activity_timeline", &crate::a11y::Action::Focus)
        .unwrap();
    driver.key("End");
    driver.key("Enter");
    assert!(driver.targets().contains_key("history-preview"));
    let events = local_events.get();
    emitter.borrow_mut().emit(()).unwrap();
    finish.set(true);
    driver.key("x");
    driver.set_modifiers(&["Ctrl".into()]);
    driver.key("z");
    driver.set_modifiers(&[]);
    assert_eq!(
        local_events.get(),
        events,
        "hidden live keyboard and undo routes are absent"
    );
    assert_eq!(starts.get(), 1);
    assert_eq!(
        completions.get(),
        1,
        "admitted effect remains attached to the live owner"
    );
    assert!(driver.click_named("live-owner").is_err());
    driver.click_named("history-back-live").unwrap();
    driver.key("y");
    assert_eq!(
        starts.get(),
        1,
        "preview cannot leave a queued local effect to run on return"
    );
    assert!(local_events.get() > events, "normal live input resumes");
}

pub(crate) struct EffectProbe {
    pub(crate) root: Box<dyn crate::widget::AppWidget>,
    pub(crate) emitter: Rc<RefCell<ActionEmitter<()>>>,
    pub(crate) starts: Rc<Cell<usize>>,
    pub(crate) finish: Rc<Cell<bool>>,
    pub(crate) completions: Rc<Cell<usize>>,
    pub(crate) collab_events: Rc<Cell<usize>>,
    pub(crate) bracket: Rc<RefCell<Option<Vec<automerge::ChangeHash>>>>,
    pub(crate) undo_request: Rc<Cell<Option<crate::undo::UndoRequest>>>,
}
pub(crate) fn native_effect_probe_parts(id: &'static str, operation: &'static str) -> EffectProbe {
    let starts = Rc::new(Cell::new(0));
    let finish = Rc::new(Cell::new(false));
    let completions = Rc::new(Cell::new(0));
    let emitter = Rc::new(RefCell::new(ActionEmitter::new(
        tilo_actions::ActionBinding::unit_for(&resource_for(id, operation), "export").unwrap(),
    )));
    let collab_events = Rc::new(Cell::new(0));
    let bracket = Rc::new(RefCell::new(None));
    let undo_request = Rc::new(Cell::new(None));
    let root = Box::new(LiveOwner {
        resource_id: id,
        type_id: operation,
        bracket: bracket.clone(),
        undo_request: undo_request.clone(),
        text: Text::new(id),
        emitter: emitter.clone(),
        starts: starts.clone(),
        finish: finish.clone(),
        completions: completions.clone(),
        collab_events: collab_events.clone(),
        local_events: Rc::new(Cell::new(0)),
        active: None,
    });
    EffectProbe {
        root,
        emitter,
        starts,
        finish,
        completions,
        collab_events,
        bracket,
        undo_request,
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
    fn permission_hint(&self) -> Option<crate::widget::AppKind> {
        None
    }
}
