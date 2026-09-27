//! A retained widget placed by hand inside a widget that draws itself.
//!
//! Canvas-style widgets (a word processor's review panel, a PDF viewer's
//! comment overlay) paint their own chrome and hand out rectangles. A child
//! that lays itself out through Taffy (a block editor, a card) still needs a
//! layout tree, node bindings and the event plumbing a host gives it.
//! [`Embedded`] is that host, owned by the parent: lay the child out in a
//! rectangle, then draw it and route events to it as usual.

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use taffy::{AvailableSpace, NodeId, Size, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{Modifiers, WindowEvent};

use super::Widget;
use crate::bound_dispatch::BoundNodeMap;
use crate::{PresenceCtx, Theme, text_render::TextRenderer};

/// A widget laid out and driven by the widget that owns it.
pub struct Embedded<W: Widget> {
    // Boxed so the node map's pointers stay valid when the host moves.
    widget: Box<W>,
    tree: TaffyTree<()>,
    nodes: HashMap<NodeId, *mut dyn Widget>,
    bindings: BoundNodeMap,
    root: Option<NodeId>,
    rect: Rect,
}

impl<W: Widget + 'static> Embedded<W> {
    #[must_use]
    pub fn new(widget: W) -> Self {
        Self {
            widget: Box::new(widget),
            tree: TaffyTree::new(),
            nodes: HashMap::new(),
            bindings: BoundNodeMap::new(),
            root: None,
            rect: Rect::ZERO,
        }
    }

    /// The hosted widget.
    pub fn widget(&self) -> &W {
        &self.widget
    }

    /// The hosted widget, mutably.
    pub fn widget_mut(&mut self) -> &mut W {
        &mut self.widget
    }

    /// Where the widget was last laid out, at the height it wanted.
    pub fn rect(&self) -> Rect {
        self.rect
    }

    fn rebuild(&mut self, theme: &Theme) {
        self.bindings.clear();
        self.nodes.clear();
        self.tree = TaffyTree::new();
        let root = self
            .widget
            .build_layout(&mut self.tree, theme, &mut self.nodes);
        // SAFETY: the boxed widget owns every node in `nodes` and stays in
        // place until the next rebuild, which clears the maps first.
        unsafe {
            self.bindings
                .rebuild(self.widget.as_ref(), &self.tree, root, &self.nodes);
        }
        self.root = Some(root);
    }

    /// Lay the widget out at `rect`'s width and origin, as tall as its
    /// content wants (at least `rect`'s height); returns that height.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "logical pixel sizes narrow to Taffy's f32"
    )]
    pub fn layout(&mut self, rect: Rect, theme: &Theme, text: &mut TextRenderer) -> f64 {
        if self.root.is_none() || self.widget.take_structure_dirty() {
            self.rebuild(theme);
        }
        let Some(root) = self.root else {
            return 0.0;
        };
        let available = Size {
            width: AvailableSpace::Definite(rect.width() as f32),
            height: AvailableSpace::MaxContent,
        };
        // SAFETY: the tree, node map and bindings were built from this exact
        // boxed widget, which nothing else borrows while the layout runs.
        let solved = unsafe {
            super::compute_widget_layout(
                &mut self.tree,
                root,
                available,
                &self.nodes,
                &self.bindings,
                text,
            )
        };
        if let Err(error) = solved {
            tracing::debug!(%error, "embedded widget layout did not settle");
        }
        self.widget
            .apply_layout(&self.tree, root, Point::new(rect.x0, rect.y0));
        let height = self
            .tree
            .layout(root)
            .map_or(0.0, |layout| f64::from(layout.size.height))
            .max(rect.height());
        self.rect = Rect::new(rect.x0, rect.y0, rect.x1, rect.y0 + height);
        height
    }

    /// Paint the widget where it was last laid out.
    pub fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.widget.draw(scene, transform, text, theme, presence);
        self.widget.mark_drawn();
    }

    /// Route one window event to the widget.
    #[allow(
        clippy::too_many_arguments,
        reason = "mirrors Widget::handle_event, minus the node and tree this host supplies"
    )]
    pub fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
    ) {
        let Some(root) = self.root else {
            return;
        };
        let mut focus = None;
        self.widget.handle_event(
            event,
            scale_factor,
            modifiers,
            state,
            path,
            handled,
            &mut focus,
            root,
            &self.tree,
        );
    }

    /// Bring the widget up to date with the document.
    pub fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
        self.widget.sync(state, path);
    }
}
