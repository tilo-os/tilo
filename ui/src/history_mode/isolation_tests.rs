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

fn resource() -> qos_actions::Resource {
    qos_actions::Resource::new("live", "fixture", "Live owner").with_action(
        qos_actions::ActionSpec::operation(
            "export",
            "Export",
            qos_actions::OperationKind::Effect,
            "fixture.export",
            &(),
        )
        .unwrap()
        .execution(qos_actions::ExecutionMode::Background { cancellable: false }),
    )
}
struct LiveOwner {
    text: Text,
    emitter: Rc<RefCell<ActionEmitter<()>>>,
    starts: Rc<Cell<usize>>,
    finish: Rc<Cell<bool>>,
    completions: Rc<Cell<usize>>,
    local_events: Rc<Cell<usize>>,
    active: Option<qos_actions::InvocationId>,
}
impl Widget for LiveOwner {
    fn name(&self) -> &str {
        "Live owner"
    }
    fn test_id(&self) -> Option<&str> {
        Some("live-owner")
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
    fn handle_local_undo_request(&mut self, _: crate::undo::UndoRequest) -> bool {
        self.local_events.set(self.local_events.get() + 1);
        true
    }
    fn resources(&self, _: &AutoCommit) -> Vec<qos_actions::Resource> {
        vec![resource()]
    }
    fn take_control_invocation(&mut self) -> Option<qos_actions::ControlInvocation> {
        self.emitter.borrow_mut().take()
    }
    fn start_resource_operation_with_document(
        &mut self,
        invocation: qos_actions::InvocationId,
        _: qos_actions::OperationKind,
        _: &str,
        _: &[u8],
        _: &mut AutoCommit,
        _: &ObjId,
    ) -> Result<Option<crate::widget::ResourceOperationStart>, String> {
        self.starts.set(self.starts.get() + 1);
        self.active = Some(invocation);
        Ok(Some(crate::widget::ResourceOperationStart::Accepted))
    }
    fn poll_resource_operation(
        &mut self,
        invocation: qos_actions::InvocationId,
        _: bool,
        _: &AutoCommit,
        _: &ObjId,
    ) -> Option<qos_actions::InvocationUpdate> {
        if self.active != Some(invocation) || !self.finish.get() {
            return None;
        }
        self.active = None;
        self.completions.set(self.completions.get() + 1);
        Some(qos_actions::InvocationUpdate::Succeeded {
            invocation,
            output: qos_actions::ActionOutput::default(),
        })
    }
}
#[test]
fn preview_discards_unadmitted_local_intents_but_polls_an_already_admitted_effect() {
    let emitter = Rc::new(RefCell::new(ActionEmitter::new(
        qos_actions::ActionBinding::unit_for(&resource(), "export").unwrap(),
    )));
    let starts = Rc::new(Cell::new(0));
    let finish = Rc::new(Cell::new(false));
    let completions = Rc::new(Cell::new(0));
    let local_events = Rc::new(Cell::new(0));
    let owner = LiveOwner {
        text: Text::new("Live owner"),
        emitter: emitter.clone(),
        starts: starts.clone(),
        finish: finish.clone(),
        completions: completions.clone(),
        local_events: local_events.clone(),
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
