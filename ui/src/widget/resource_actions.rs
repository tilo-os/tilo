//! Toolkit-owned host for a small app's typed resource/action surface.
//!
//! Apps provide their retained root, one discoverable resource, and an
//! immediate or background operation closure. The host supplies the widget
//! forwarding and invocation lifecycle; scaffolds do not need to copy that
//! infrastructure into every new project.
use std::collections::HashMap;
use std::sync::mpsc::{Receiver, TryRecvError};

use crate::automerge::{AutoCommit, ObjId};
use crate::kurbo::{Affine, Point, Rect};
use crate::qos_actions::{
    ActionContract, ActionFailure, ActionOutput, ActionParam, ActionSpec, DiscoveryScope,
    EncodedAction, ExecutionMode, InvocationId, InvocationUpdate, OperationKind, Resource,
    ResultSpec,
};
use crate::taffy::{NodeId, TaffyTree};
use crate::vello::Scene;
use crate::widget::{ResourceOperationStart, TaffyTreeExt, Widget};

/// Shared effect-operation type used by the one-resource app host.
pub const OPERATION_TYPE: &str = "qos.app.operation";

/// Build one launchable resource with a unit operation.
///
/// # Panics
///
/// Panics only if serializing Rust's unit value fails, which the JSON encoder
/// represents unconditionally as `null`.
#[expect(
    clippy::expect_used,
    reason = "serializing the unit operation payload is infallible"
)]
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
    .expect("the unit operation is serializable");
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

/// Build one launchable resource whose arguments are schema-bound.
///
/// # Panics
///
/// Panics only if the already-validated JSON argument object cannot be encoded
/// as JSON again.
#[expect(
    clippy::expect_used,
    reason = "the bound arguments are already a serde_json value"
)]
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
                .expect("bound arguments are serializable")
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

/// Transparent retained root that publishes and executes one app resource.
pub struct ResourceActionHost {
    root: Box<dyn Widget>,
    resource: Resource,
    runner: Runner,
    pending: HashMap<InvocationId, Receiver<Result<ActionOutput, String>>>,
    rect: Rect,
}

/// Wrap a root with a synchronous typed resource operation.
pub fn immediate(
    root: Box<dyn Widget>,
    resource: Resource,
    run: impl FnMut(&[u8]) -> Result<ActionOutput, String> + 'static,
) -> Box<dyn Widget> {
    Box::new(ResourceActionHost {
        root,
        resource,
        runner: Runner::Immediate(Box::new(run)),
        pending: HashMap::new(),
        rect: Rect::ZERO,
    })
}

/// Wrap a root with an accepted job whose receiver supplies terminal output.
pub fn background(
    root: Box<dyn Widget>,
    resource: Resource,
    run: impl FnMut(&[u8]) -> Result<Receiver<Result<ActionOutput, String>>, String> + 'static,
) -> Box<dyn Widget> {
    Box::new(ResourceActionHost {
        root,
        resource,
        runner: Runner::Background(Box::new(run)),
        pending: HashMap::new(),
        rect: Rect::ZERO,
    })
}

impl Widget for ResourceActionHost {
    fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
        self.root.sync(state, path);
    }

    fn update(&mut self, state: &mut AutoCommit, path: &ObjId) -> bool {
        self.root.update(state, path)
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &crate::Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let child = self.root.build_layout(taffy, theme, node_map);
        let node = taffy.parent_node(
            crate::taffy::Style {
                size: crate::taffy::Size {
                    width: crate::taffy::style_helpers::percent(1.0_f32),
                    height: crate::taffy::style_helpers::percent(1.0_f32),
                },
                ..Default::default()
            },
            &[child],
        );
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
        text_renderer: &mut crate::text_render::TextRenderer,
        theme: &crate::Theme,
        presence: &crate::presence::PresenceCtx,
    ) {
        self.root
            .draw(scene, transform, text_renderer, theme, presence);
    }

    fn handle_event(
        &mut self,
        event: &crate::winit::event::WindowEvent,
        scale_factor: f64,
        modifiers: &crate::winit::event::Modifiers,
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

    fn is_enabled(&self) -> bool {
        self.root.is_enabled()
    }

    fn ime_cursor_rect(&self) -> Option<Rect> {
        self.root.ime_cursor_rect()
    }

    fn ime_purpose(&self) -> Option<crate::winit::window::ImePurpose> {
        self.root.ime_purpose()
    }

    fn traps_focus(&self) -> bool {
        self.root.traps_focus()
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        self.root.accessibility()
    }

    fn synthetic_accessibility_rect(&self, path: &[usize]) -> Option<Rect> {
        self.root.synthetic_accessibility_rect(path)
    }

    fn perform_action(
        &mut self,
        action: &crate::a11y::Action,
        state: &mut AutoCommit,
        path: &ObjId,
    ) -> bool {
        self.root.perform_action(action, state, path)
    }

    fn perform_action_at(
        &mut self,
        action: &crate::a11y::Action,
        key_path: &[u64],
        state: &mut AutoCommit,
        path: &ObjId,
    ) -> bool {
        self.root.perform_action_at(action, key_path, state, path)
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &crate::a11y::Action,
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
            return self
                .root
                .start_resource_operation(invocation, kind, type_id, payload, doc, path);
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
                    code: "app_operation_failed".into(),
                    message,
                    details: None,
                    retryable: false,
                },
            },
        })
    }

    fn wants_pointer_capture(&self) -> bool {
        self.root.wants_pointer_capture()
    }

    fn needs_animation_frame(&self) -> bool {
        self.root.needs_animation_frame()
    }

    fn next_wake_deadline(&self) -> Option<std::time::Instant> {
        self.root.next_wake_deadline()
    }

    fn take_structure_dirty(&mut self) -> bool {
        self.root.take_structure_dirty()
    }

    fn needs_redraw(&self) -> bool {
        self.root.needs_redraw()
    }

    fn mark_drawn(&mut self) {
        self.root.mark_drawn();
    }

    fn is_layout_dirty(&self) -> bool {
        self.root.is_layout_dirty()
    }

    fn layout_needs_measure(&self) -> bool {
        self.root.layout_needs_measure()
    }

    fn name(&self) -> &'static str {
        "ResourceActionHost"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}
