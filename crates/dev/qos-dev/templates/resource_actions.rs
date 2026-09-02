/// Generated resource/action host shared by every qOS application template.

use std::collections::HashMap;
use std::sync::mpsc::{Receiver, TryRecvError};

use ui::automerge::{AutoCommit, ObjId};
use ui::kurbo::{Affine, Point, Rect};
use ui::qos_actions::{
    ActionContract, ActionFailure, ActionOutput, ActionParam, ActionSpec, DiscoveryScope,
    EncodedAction, ExecutionMode, InvocationId, InvocationUpdate, OperationKind, Resource,
    ResultSpec,
};
use ui::taffy::{NodeId, TaffyTree};
use ui::vello::Scene;
use ui::widget::{ResourceOperationStart, Widget};

pub const OPERATION_TYPE: &str = "generated.app.operation";

pub fn resource(
    kind: &str,
    label: &str,
    action_id: &str,
    action_label: &str,
    background: bool,
) -> Resource {
    let mut action = ActionSpec::operation(
        action_id,
        action_label,
        OperationKind::Effect,
        OPERATION_TYPE,
        &(),
    )
    .expect("the generated unit operation is serializable");
    action.contract = ActionContract {
        discovery: DiscoveryScope::Launchable,
        execution: if background {
            ExecutionMode::Background { cancellable: false }
        } else {
            ExecutionMode::Immediate
        },
        result: if background {
            ResultSpec::Job
        } else {
            ResultSpec::None
        },
        ..ActionContract::default()
    };
    Resource::new(format!("{kind}:root"), kind, label).with_action(action)
}

pub fn typed_resource(
    kind: &str,
    label: &str,
    action_id: &str,
    action_label: &str,
    params: Vec<ActionParam>,
) -> Resource {
    let mut action = ActionSpec::typed_operation(
        action_id,
        action_label,
        "Enter action arguments",
        OperationKind::Effect,
        OPERATION_TYPE,
        params,
        |args| {
            EncodedAction::encode_operation(OPERATION_TYPE, &args.to_json())
                .expect("bound generated arguments are serializable")
        },
    );
    action.contract.discovery = DiscoveryScope::Launchable;
    Resource::new(format!("{kind}:root"), kind, label).with_action(action)
}

type Immediate = Box<dyn FnMut(&[u8]) -> Result<ActionOutput, String>>;
type Background = Box<dyn FnMut(&[u8]) -> Result<Receiver<Result<ActionOutput, String>>, String>>;

enum Runner {
    Immediate(Immediate),
    Background(Background),
}

pub struct ResourceActionApp {
    root: Box<dyn Widget>,
    resource: Resource,
    runner: Runner,
    pending: HashMap<InvocationId, Receiver<Result<ActionOutput, String>>>,
    rect: Rect,
}

pub fn immediate(
    root: Box<dyn Widget>,
    resource: Resource,
    run: impl FnMut(&[u8]) -> Result<ActionOutput, String> + 'static,
) -> Box<dyn Widget> {
    Box::new(ResourceActionApp {
        root,
        resource,
        runner: Runner::Immediate(Box::new(run)),
        pending: HashMap::new(),
        rect: Rect::ZERO,
    })
}

pub fn background(
    root: Box<dyn Widget>,
    resource: Resource,
    run: impl FnMut(&[u8]) -> Result<Receiver<Result<ActionOutput, String>>, String> + 'static,
) -> Box<dyn Widget> {
    Box::new(ResourceActionApp {
        root,
        resource,
        runner: Runner::Background(Box::new(run)),
        pending: HashMap::new(),
        rect: Rect::ZERO,
    })
}

impl Widget for ResourceActionApp {
    fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
        self.root.sync(state, path);
    }

    fn update(&mut self, state: &mut AutoCommit, path: &ObjId) -> bool {
        self.root.update(state, path)
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &ui::Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let child = self.root.build_layout(taffy, theme, node_map);
        let node = taffy
            .new_with_children(
                ui::taffy::Style {
                    size: ui::taffy::Size {
                        width: ui::taffy::style_helpers::percent(1.0_f32),
                        height: ui::taffy::style_helpers::percent(1.0_f32),
                    },
                    ..Default::default()
                },
                &[child],
            )
            .expect("generated resource host layout is valid");
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x + layout.size.width),
            origin.y + f64::from(layout.location.y + layout.size.height),
        );
        if let Ok(children) = taffy.children(node)
            && let Some(child) = children.first()
        {
            self.root
                .apply_layout(taffy, *child, Point::new(self.rect.x0, self.rect.y0));
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &ui::Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        self.root
            .draw(scene, transform, text_renderer, theme, presence);
    }

    fn handle_event(
        &mut self,
        event: &ui::winit::event::WindowEvent,
        scale_factor: f64,
        modifiers: &ui::winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        self.root.handle_event(
            event,
            scale_factor,
            modifiers,
            state,
            path,
            handled,
            focus_target,
            taffy
                .children(node_id)
                .ok()
                .and_then(|children| children.first().copied())
                .unwrap_or(node_id),
            taffy,
        );
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![self.root.as_ref()]
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![self.root.as_mut()]
    }

    fn is_focusable(&self) -> bool {
        self.root.is_focusable()
    }

    fn set_focus(&mut self, focused: bool) {
        self.root.set_focus(focused);
    }

    fn is_focused(&self) -> bool {
        self.root.is_focused()
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        self.root.accessibility()
    }

    fn synthetic_accessibility_rect(&self, path: &[usize]) -> Option<Rect> {
        self.root.synthetic_accessibility_rect(path)
    }

    fn perform_action(
        &mut self,
        action: &ui::a11y::Action,
        state: &mut AutoCommit,
        path: &ObjId,
    ) -> bool {
        self.root.perform_action(action, state, path)
    }

    fn perform_action_at(
        &mut self,
        action: &ui::a11y::Action,
        key_path: &[u64],
        state: &mut AutoCommit,
        path: &ObjId,
    ) -> bool {
        self.root.perform_action_at(action, key_path, state, path)
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        state: &mut AutoCommit,
        path: &ObjId,
    ) -> bool {
        self.root
            .perform_semantic_action(target, action, state, path)
    }

    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[OPERATION_TYPE]
    }

    fn resources(&self, _doc: &AutoCommit) -> Vec<Resource> {
        vec![self.resource.clone()]
    }

    fn invoke_resource_operation_v2(
        &mut self,
        kind: OperationKind,
        type_id: &str,
        payload: &[u8],
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Result<Option<ActionOutput>, String> {
        if kind == OperationKind::Document || type_id != OPERATION_TYPE {
            return Ok(None);
        }
        match &mut self.runner {
            Runner::Immediate(run) => run(payload).map(Some),
            Runner::Background(_) => Ok(None),
        }
    }

    fn start_resource_operation(
        &mut self,
        invocation: InvocationId,
        kind: OperationKind,
        type_id: &str,
        payload: &[u8],
        doc: &AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ResourceOperationStart>, String> {
        if kind == OperationKind::Document || type_id != OPERATION_TYPE {
            return self.root.start_resource_operation(
                invocation, kind, type_id, payload, doc, path,
            );
        }
        match &mut self.runner {
            Runner::Immediate(run) => run(payload)
                .map(ResourceOperationStart::Completed)
                .map(Some),
            Runner::Background(run) => {
                self.pending.insert(invocation, run(payload)?);
                Ok(Some(ResourceOperationStart::Accepted))
            }
        }
    }

    fn poll_resource_operation(
        &mut self,
        invocation: InvocationId,
        _cancellation_requested: bool,
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Option<InvocationUpdate> {
        let result = match self.pending.get(&invocation)?.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => Err("background worker disconnected".into()),
        };
        self.pending.remove(&invocation);
        Some(match result {
            Ok(output) => InvocationUpdate::Succeeded { invocation, output },
            Err(message) => InvocationUpdate::Failed {
                invocation,
                error: ActionFailure {
                    code: "generated_operation_failed".into(),
                    message,
                    details: None,
                    retryable: false,
                },
            },
        })
    }

    fn name(&self) -> &str {
        "ResourceActionApp"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}
