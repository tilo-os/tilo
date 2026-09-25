//! The contract a custom widget or renderer implements to take part in a
//! resource: collaboration, history, undo, permissions and the action door.

use std::collections::HashMap;
use std::rc::Rc;

use taffy::{NodeId, TaffyTree};
use tilo_resource::Resource;
use ui_core::automerge::{AutoCommit, ObjId};
use ui_core::presence::{PresenceCtx, PresenceKey};
use ui_core::text_render::TextRenderer;
use ui_core::vello::Scene;
use ui_core::vello::kurbo::{Affine, Point, Rect};
use ui_core::widget::Widget;
use ui_core::winit::event::{Modifiers, WindowEvent};
use ui_core::{PresencePayload, Theme, taffy};

use super::dispatch::Dispatcher;
use super::live::Live;

/// A custom widget or renderer — a timeline, a canvas, a GPU viewport, a page
/// layout engine — taking part in a resource.
///
/// Pixels, layout and input stay the toolkit's `Widget` contract, unchanged.
/// This trait adds only what the framework cannot infer:
///
/// * **Read** — [`present`](Self::present) receives the resource state
///   whenever it changes: a local edit, a collaborator's, an undo. In History
///   mode the framework builds the surface again from a read-only `ViewCx`
///   and presents a *past* version, so time travel needs no extra code as
///   long as the surface draws from what it was presented.
/// * **Write** — edits go through the [`Dispatcher`] handed to
///   [`attach`](Self::attach): a named action, or any typed action value via
///   [`Dispatcher::apply`]. The host checks permissions, applies, records one
///   undo step per action, attributes it in history and replicates it. A
///   surface never writes the document.
/// * **Undo granularity** — one action is one undo step. Preview a gesture
///   locally (a drag, a scrub) and dispatch one action when it ends.
/// * **Permissions** — [`SurfaceCx::read_only`] says the participant may
///   only view; present accordingly. Refused actions change nothing.
/// * **Confirmation** — a typed value whose
///   [`risk`](tilo_actions::Action::risk) is destructive (deleting a clip)
///   waits for the user's confirmation, like a declared destructive action.
///   When an edit is refused or held for confirmation, the surface is
///   presented the stored state again, undoing any optimistic preview; a
///   confirmed edit then arrives as an ordinary change.
/// * **Presence** — report where the user is as a [`ResourceSelection`] from
///   the toolkit's `Widget::presence_payload` (one line:
///   `selection.presence::<R>(app_id)`), and draw collaborators from the
///   `PresenceCtx` every draw receives, under the same keys.
pub trait ResourceSurface<R: Resource>: Widget {
    /// The resource state to show.
    fn present(&mut self, state: &Rc<R::State>);

    /// The channel edits go through, and whether they will be accepted.
    /// Called once, before the first [`present`](Self::present).
    fn attach(&mut self, cx: SurfaceCx<R>) {
        let _ = cx;
    }
}

/// What a surface receives when attached.
pub struct SurfaceCx<R: Resource> {
    pub(crate) dispatch: Dispatcher<R>,
}

impl<R: Resource> SurfaceCx<R> {
    pub fn dispatcher(&self) -> Dispatcher<R> {
        self.dispatch.clone()
    }

    pub fn read_only(&self) -> bool {
        self.dispatch.read_only()
    }
}

/// Where a participant is inside a resource, in the resource's own terms, so
/// it means the same thing on every collaborator's screen.
#[derive(Clone, Debug, PartialEq)]
pub enum ResourceSelection {
    /// An item (a clip, a card, a row) is selected.
    Item(String),
    /// A caret at a position inside an item (a text field, a cell).
    Caret { item: String, at: usize },
    /// A range inside an item: characters of a field, frames of a clip.
    Range {
        item: String,
        start: usize,
        end: usize,
    },
}

impl ResourceSelection {
    /// The toolkit presence frame for this selection, keyed to the resource
    /// item so peers resolve it against their own layout.
    pub fn presence<R: Resource>(&self, app_id: &str) -> (PresenceKey, PresencePayload) {
        let key = |item: &str| PresenceKey::Resource {
            app: app_id.to_owned(),
            id: item.to_owned(),
            kind: R::KIND.to_owned(),
        };
        match self {
            Self::Item(item) => (key(item), PresencePayload::Selection { start: 0, end: 0 }),
            Self::Caret { item, at } => (key(item), PresencePayload::Caret { pos: *at }),
            Self::Range { item, start, end } => (
                key(item),
                PresencePayload::Selection {
                    start: *start,
                    end: *end,
                },
            ),
        }
    }
}

/// Presents resource changes to a hosted surface and otherwise stays out of
/// its way: layout, drawing and input are the surface's own.
pub(crate) struct SurfaceHost<R: Resource, S> {
    live: Live<R>,
    presented: u64,
    surface: S,
}

impl<R: Resource, S: ResourceSurface<R>> SurfaceHost<R, S> {
    pub(crate) fn new(live: Live<R>, surface: S) -> Self {
        Self {
            presented: live.revision(),
            live,
            surface,
        }
    }
}

impl<R: Resource, S: ResourceSurface<R>> Widget for SurfaceHost<R, S> {
    fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
        if self.live.revision() != self.presented {
            self.presented = self.live.revision();
            self.surface.present(&self.live.get());
        }
        self.surface.sync(state, path);
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.surface.build_layout(taffy, theme, node_map)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.surface.apply_layout(taffy, node, origin);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.surface.draw(scene, transform, text, theme, presence);
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        modifiers: &Modifiers,
        doc: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus: &mut Option<NodeId>,
        node: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        self.surface.handle_event(
            event, scale, modifiers, doc, path, handled, focus, node, taffy,
        );
    }

    fn wants_pointer_capture(&self) -> bool {
        self.surface.wants_pointer_capture()
    }

    // A transparent container forwards document-capable operation admission
    // to the handlers it hosts (see the toolkit's `Widget` docs).
    fn start_resource_operation_with_document(
        &mut self,
        invocation: tilo_actions::InvocationId,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        doc: &mut AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui_core::widget::ResourceOperationStart>, String> {
        for child in self.children_mut() {
            if let Some(started) = child.start_resource_operation_with_document(
                invocation, kind, type_id, payload, doc, path,
            )? {
                return Ok(Some(started));
            }
        }
        Ok(None)
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![&self.surface]
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![&mut self.surface]
    }

    fn name(&self) -> &'static str {
        "ResourceSurface"
    }

    fn get_rect(&self) -> Rect {
        self.surface.get_rect()
    }
}
