//! The contract a custom widget or renderer implements to take part in a
//! resource: collaboration, history, undo, permissions and the action door.

use std::rc::Rc;

use tilo_resource::Resource;
use ui_core::PresencePayload;
use ui_core::automerge::{AutoCommit, ObjId};
use ui_core::presence::PresenceKey;
use ui_core::widget::{Decorator, Widget};

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

// The host adds one thing to the surface — presenting new resource state
// before the surface syncs — and forwards everything else to it.
impl<R: Resource, S: ResourceSurface<R>> Decorator for SurfaceHost<R, S> {
    type Inner = S;

    fn inner(&self) -> &S {
        &self.surface
    }

    fn inner_mut(&mut self) -> &mut S {
        &mut self.surface
    }

    fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
        if self.live.revision() != self.presented {
            self.presented = self.live.revision();
            self.surface.present(&self.live.get());
        }
        self.surface.sync(state, path);
    }
}
