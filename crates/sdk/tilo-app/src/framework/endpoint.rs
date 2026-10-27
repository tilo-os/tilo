//! An effect vocabulary answered by a widget of the window, for a host that runs documents of
//! its own.
//!
//! A [`ResourceApp`](super::ResourceApp) answers its effects through its views and surface. A
//! host whose window is its own widget (tilolang runs other apps' packages) has no such view; it
//! mounts an [`EffectEndpoint`] anywhere in its tree and publishes the vocabulary's specs on a
//! resource itself. The endpoint answers each invocation at once, as the typed value it
//! decoded, with who invoked it; it starts no job and changes no document by itself, so what an invocation changes in
//! the room it changes through the host's own commits.

use std::collections::HashMap;

use tilo_actions::ActionOutput;
use tilo_resource::Effect;
use ui_core::host_operation::{ResourceOperation, ResourceOperationStart, ResourceOperations};
use ui_core::taffy::{NodeId, Style, TaffyTree};
use ui_core::theme::Theme;
use ui_core::vello::kurbo::{Point, Rect};
use ui_core::widget::{Edits, TaffyTreeExt as _, Widget};

/// Who invoked an effect: the invocation, and the invoker the door admitted for it. `None` is the
/// person at this window acting for themselves; an agent's task is its delegate with the person
/// it acts for.
#[derive(Clone, Debug)]
pub struct EffectCaller {
    pub invocation: tilo_actions::InvocationId,
    pub invoker: Option<tilo_actions::ActionInvoker>,
}

type Handler<E> = Box<dyn FnMut(E, &EffectCaller) -> Result<ActionOutput, String>>;

/// A widget that takes no room and answers the vocabulary `E`.
pub struct EffectEndpoint<E: Effect> {
    handle: Handler<E>,
    /// The operation type of `E`, in the form the host's handler registry reads.
    handled: &'static [&'static str],
    rect: Rect,
}

impl<E: Effect> EffectEndpoint<E> {
    /// An endpoint that answers each effect with `handle`.
    pub fn new(
        handle: impl FnMut(E, &EffectCaller) -> Result<ActionOutput, String> + 'static,
    ) -> Self {
        Self {
            handle: Box::new(handle),
            // One small allocation per endpoint, which lives as long as its window.
            handled: Box::leak(Box::new([E::OPERATION])),
            rect: Rect::ZERO,
        }
    }
}

impl<E: Effect> Widget for EffectEndpoint<E> {
    ui_core::provides!(dyn ResourceOperations);

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.widget_leaf(
            Style {
                display: ui_core::taffy::Display::None,
                ..Style::default()
            },
            node_map,
            self,
        )
    }

    fn apply_layout(&mut self, _taffy: &TaffyTree<()>, _node: NodeId, origin: Point) {
        self.rect = Rect::from_origin_size(origin, (0.0, 0.0));
    }

    fn needs_redraw(&self) -> bool {
        false
    }

    fn is_layout_dirty(&self) -> bool {
        false
    }

    fn name(&self) -> &'static str {
        "EffectEndpoint"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

impl<E: Effect> ResourceOperations for EffectEndpoint<E> {
    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        self.handled
    }

    fn start_operation(
        &mut self,
        op: &ResourceOperation<'_>,
        _doc: &mut Edits<'_>,
        _path: &ui_core::automerge::ObjId,
    ) -> Result<Option<ResourceOperationStart>, String> {
        let (request, _custody) = op.with_custody();
        if request.type_id != E::OPERATION {
            return Ok(None);
        }
        op.check()?;
        let effect = E::from_payload(request.payload)?;
        let caller = EffectCaller {
            invocation: request.invocation,
            invoker: ui_core::invocation_scope::current_invoker(),
        };
        (self.handle)(effect, &caller).map(|output| Some(ResourceOperationStart::Completed(output)))
    }
}
