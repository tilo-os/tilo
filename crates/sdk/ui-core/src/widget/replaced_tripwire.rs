//! A test fixture for "no stale pointer is followed": a container whose
//! child is replaced on demand, at sync, at settle or while it handles an
//! event. The replaced child is kept alive in a graveyard (so a stale map
//! entry still points at valid memory) and armed: anything that reaches it
//! afterwards — through a layout map that was not rebuilt — panics, instead
//! of reading freed memory as a real replacement would.

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

use taffy::{NodeId, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::{AppModel, EventCtx, ModelPath, TaffyTreeExt, Widget};

const REACHED: &str = "a replaced widget was reached through a layout map that was not rebuilt";

/// A focusable leaf that panics once it was replaced.
pub(crate) struct Tripwire {
    replaced: Rc<Cell<bool>>,
    focused: bool,
    rect: Rect,
}

impl Tripwire {
    fn new(focused: bool) -> (Self, Rc<Cell<bool>>) {
        let replaced = Rc::new(Cell::new(false));
        (
            Self {
                replaced: Rc::clone(&replaced),
                focused,
                rect: Rect::ZERO,
            },
            replaced,
        )
    }

    fn check(&self) {
        assert!(!self.replaced.get(), "{REACHED}");
    }
}

impl Widget for Tripwire {
    fn provide<'a>(&'a self, _request: &mut crate::widget::capability::Request<'a, '_>) {
        self.check();
    }
    fn provide_mut<'a>(&'a mut self, _request: &mut crate::widget::capability::RequestMut<'a, '_>) {
        self.check();
    }
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy.leaf_node(taffy::Style::default());
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }
    fn apply_layout(&mut self, _taffy: &TaffyTree<()>, _node: NodeId, origin: Point) {
        self.rect = Rect::from_origin_size(origin, (80.0, 24.0));
    }
    fn draw(&mut self, _: &mut Scene, _: Affine, _: &mut TextRenderer, _: &Theme, _: &PresenceCtx) {
        self.check();
    }
    fn get_rect(&self) -> Rect {
        self.check();
        self.rect
    }
    fn is_focusable(&self) -> bool {
        self.check();
        true
    }
    fn is_focused(&self) -> bool {
        self.check();
        self.focused
    }
    fn set_focus(&mut self, focused: bool) {
        self.check();
        self.focused = focused;
    }
    fn is_enabled(&self) -> bool {
        self.check();
        true
    }
    fn test_id(&self) -> Option<&str> {
        self.check();
        Some("tripwire")
    }
    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        self.check();
        Some(crate::a11y::AccessNode::new(crate::a11y::Role::Button).with_name("Tripwire"))
    }
    fn name(&self) -> &'static str {
        "Tripwire"
    }
}

/// When the container replaces its child.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReplaceAt {
    Sync,
    Settle,
    /// While it handles an event, leaving the event unhandled.
    Event,
}

/// A container whose focusable child is replaced once `armed` is set, at
/// the moment `at` names.
pub(crate) struct Replacing {
    child: Box<dyn Widget>,
    child_replaced: Rc<Cell<bool>>,
    graveyard: Vec<Box<dyn Widget>>,
    at: ReplaceAt,
    armed: Rc<Cell<bool>>,
    dirty: bool,
    /// Whether it reports the replacement, as every widget must; one that
    /// forgets may leave a stale layout, never a call into a replaced child.
    reports: bool,
    rect: Rect,
}

impl Replacing {
    /// The container with a focused child, and the switch that arms it.
    pub(crate) fn new(at: ReplaceAt) -> (Self, Rc<Cell<bool>>) {
        let (child, child_replaced) = Tripwire::new(true);
        let armed = Rc::new(Cell::new(false));
        (
            Self {
                child: Box::new(child),
                child_replaced,
                graveyard: Vec::new(),
                at,
                armed: Rc::clone(&armed),
                dirty: false,
                reports: true,
                rect: Rect::ZERO,
            },
            armed,
        )
    }

    /// [`Self::new`], but it never reports the replacement.
    pub(crate) fn unreported(at: ReplaceAt) -> (Self, Rc<Cell<bool>>) {
        let (mut replacing, armed) = Self::new(at);
        replacing.reports = false;
        (replacing, armed)
    }

    fn replace_if(&mut self, now: ReplaceAt) {
        if self.at != now || !self.armed.replace(false) {
            return;
        }
        let (fresh, replaced) = Tripwire::new(false);
        self.child_replaced.set(true);
        self.child_replaced = replaced;
        let old = std::mem::replace(&mut self.child, Box::new(fresh));
        self.graveyard.push(old);
        self.dirty = self.reports;
    }
}

impl Widget for Replacing {
    fn sync(&mut self, _state: &AppModel, _path: &ModelPath) {
        self.replace_if(ReplaceAt::Sync);
    }
    fn settle(&mut self, _edits: &mut crate::widget::Edits<'_>, _path: &automerge::ObjId) {
        self.replace_if(ReplaceAt::Settle);
    }
    fn handle_event(&mut self, _event: &WindowEvent, _ctx: &mut EventCtx<'_>) {
        self.replace_if(ReplaceAt::Event);
    }
    fn take_structure_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let child = self.child.build_layout(taffy, theme, node_map);
        taffy.widget_node(taffy::Style::default(), &[child], node_map, self)
    }
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = Rect::from_origin_size(origin, (200.0, 100.0));
        if let Ok(children) = taffy.children(node)
            && let Some(child) = children.first()
        {
            self.child.apply_layout(taffy, *child, origin);
        }
    }
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.child.draw(scene, transform, text, theme, presence);
    }
    fn children(&self) -> Vec<&dyn Widget> {
        vec![self.child.as_ref()]
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![self.child.as_mut()]
    }
    fn name(&self) -> &'static str {
        "Replacing"
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
}
