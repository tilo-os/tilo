//! The session's layers in the page: what a role of the session places over the
//! desk — who is here, with its cards and everyone else's pointer — drawn by the
//! page's own frame.
//!
//! [`tilo_session::platform::surfaces`] is the seam: a role draws its panel as a
//! widget tree of its own and asks its host to show it on a layer above the
//! windows. A native session's compositor draws each such tree on a surface of
//! its own; the page has one canvas, so a layer is a widget tree standing over
//! the desk in the page's own tree, and the page's semantic projection carries
//! what it shows to assistive technology.

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

use tilo_session::platform::surfaces::{Heard, Layer, Placement, Surfaces};
use ui::kurbo::{Affine, Point, Rect};
use ui::presence::PresenceCtx;
use ui::taffy::{LengthPercentageAuto, NodeId, Position, Style, TaffyTree};
use ui::vello::Scene;
use ui::widget::DrawChild;
use ui::widget::{EventCtx, TaffyTreeExt as _, Widget};
use ui::winit::event::WindowEvent;
use ui::{TextRenderer, Theme};

/// One layer, as the role that placed it holds it: what it asks the page for.
struct Handle {
    /// The frames it drew, as the role reads what it shows.
    revision: Rc<Cell<u64>>,
    /// The role changed what it shows without input reaching it: the page's next pass lays it out
    /// again.
    asked: Rc<Cell<bool>>,
    /// Ask the page for that frame.
    wake: Rc<dyn Fn()>,
}

impl Layer for Handle {
    fn relayout(&self) {
        self.asked.set(true);
        (self.wake)();
    }

    /// The page's frames draw in the page's theme, which the mount takes from the person's
    /// settings: the layer draws again in it.
    fn set_theme(&self, _theme: Theme) {
        self.relayout();
    }

    fn revision(&self) -> u64 {
        self.revision.get()
    }

    /// A layer is in the page's own tree, so what it shows assistive technology is the page's
    /// semantic projection of that tree: there is no tree of its own to hand over here.
    fn a11y_tree(&self) -> Option<ui::accesskit::TreeUpdate> {
        None
    }

    fn a11y_action(&self, _target: ui::accesskit::NodeId, _action: &ui::a11y::Action) -> bool {
        false
    }

    fn accessible_labels(&self) -> Vec<String> {
        Vec::new()
    }

    fn controls(&self) -> Vec<ui::window_targets::WindowControl> {
        Vec::new()
    }

    fn press(&self, name: &str) -> Result<(), String> {
        Err(format!("no control named {name}"))
    }
}

/// A layer in the page's tree: the shell draws it, routes the input that lands on it, and hands it
/// to assistive technology as any other child.
pub(crate) struct LayerWidget {
    /// The role's widget tree, out of the page's flow over the whole desk.
    root: Box<dyn Widget>,
    /// What the role hears from the page, beside the input its tree is given.
    heard: Heard,
    revision: Rc<Cell<u64>>,
    asked: Rc<Cell<bool>>,
}

impl LayerWidget {
    fn wants_pointer_capture(&self) -> bool {
        self.root.wants_pointer_capture()
    }

    pub(crate) fn wants_keyboard(&self) -> bool {
        (self.heard.wants_keyboard)()
    }

    /// Whether the role draws something pressed at `point` of the bar, which the bar hands to it.
    pub(crate) fn takes_bar_point(&self, point: Point) -> bool {
        (self.heard.takes_bar_point)(point)
    }

    /// The pointer was pressed somewhere else: what the role has open closes.
    pub(crate) fn pressed_elsewhere(&self) {
        (self.heard.pressed_elsewhere)();
    }
}

impl Widget for LayerWidget {
    /// A role that changed what it shows, though no input reached it, is laid out again.
    fn update(&mut self, _state: &ui::widget::AppModel, _path: &ui::widget::ModelPath) -> bool {
        self.asked.replace(false)
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let root = self.root.build_layout(taffy, theme, map);
        // Over the whole desk, out of the page's flow: the role's tree covers what it draws on.
        let style = Style {
            position: Position::Absolute,
            inset: ui::taffy::Rect {
                left: LengthPercentageAuto::length(0.0),
                right: LengthPercentageAuto::length(0.0),
                top: LengthPercentageAuto::length(0.0),
                bottom: LengthPercentageAuto::length(0.0),
            },
            ..Default::default()
        };
        taffy.widget_node(style, &[root], map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let inner = ui::widget::layout_rect(taffy, node, origin);
        if let Some(root) = taffy.children(node).unwrap_or_default().first().copied() {
            self.root.apply_layout(taffy, root, inner.origin());
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
        self.root
            .draw_child(scene, transform, text, theme, presence);
        self.revision.set(self.revision.get().wrapping_add(1));
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        let Some(&root) = ctx.taffy.children(ctx.node_id).unwrap_or_default().first() else {
            return;
        };
        self.root
            .handle_event(event, &mut ctx.reborrow(ctx.path, root));
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![self.root.as_ref()]
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![self.root.as_mut()]
    }

    fn take_structure_dirty(&mut self) -> bool {
        self.root.take_structure_dirty()
    }

    fn needs_redraw(&self) -> bool {
        self.root.needs_redraw()
    }

    fn needs_animation_frame(&self) -> bool {
        self.root.needs_animation_frame()
    }

    fn next_wake_deadline(&self) -> Option<ui::time::Instant> {
        self.root.next_wake_deadline()
    }

    fn wants_pointer_capture(&self) -> bool {
        self.root.wants_pointer_capture()
    }

    fn traps_focus(&self) -> bool {
        self.root.traps_focus()
    }

    fn modal_capture(&self) -> Option<Rect> {
        self.root.modal_capture()
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        self.root.accessibility()
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        state: &mut ui::widget::Edits<'_>,
        path: &ui::widget::ModelPath,
    ) -> bool {
        self.root
            .perform_semantic_action(target, action, state, path)
    }

    fn is_layout_dirty(&self) -> bool {
        self.root.is_layout_dirty()
    }

    fn name(&self) -> &'static str {
        "SessionLayer"
    }

    fn get_rect(&self) -> Rect {
        self.root.get_rect()
    }
}

/// The layers the session placed over the page, in the order it placed them: the shell draws them
/// above the desk and below its own sheets.
#[derive(Default)]
pub(crate) struct Layers {
    placed: Vec<LayerWidget>,
}

impl Layers {
    /// What the session places its layers on, asking the page for the frame each one shows in.
    pub(crate) fn placing(&mut self, wake: Rc<dyn Fn()>) -> Placing<'_> {
        Placing { layers: self, wake }
    }

    /// Whether a layer's role takes the pointer where it stands: a wheel over it is the role's,
    /// never the desk's.
    pub(crate) fn wants_pointer_capture(&self) -> bool {
        self.placed.iter().any(LayerWidget::wants_pointer_capture)
    }

    /// The pointer was pressed somewhere no layer stands: what a role has open closes.
    pub(crate) fn pressed_elsewhere(&self) {
        for layer in &self.placed {
            layer.pressed_elsewhere();
        }
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &LayerWidget> {
        self.placed.iter()
    }

    pub(crate) fn iter_mut(&mut self) -> impl Iterator<Item = &mut LayerWidget> {
        self.placed.iter_mut()
    }
}

/// A session placing its layers while the page builds the shell that draws them.
pub(crate) struct Placing<'a> {
    layers: &'a mut Layers,
    wake: Rc<dyn Fn()>,
}

impl Surfaces for Placing<'_> {
    fn place(
        &mut self,
        _name: &str,
        tree: Box<dyn Widget>,
        _placement: Placement,
        heard: Heard,
    ) -> Rc<dyn Layer> {
        let revision = Rc::new(Cell::new(0));
        let asked = Rc::new(Cell::new(false));
        self.layers.placed.push(LayerWidget {
            root: tree,
            heard,
            revision: Rc::clone(&revision),
            asked: Rc::clone(&asked),
        });
        Rc::new(Handle {
            revision,
            asked,
            wake: Rc::clone(&self.wake),
        })
    }

    /// The page has one tree and one publication of what it shows: a role's layer is drawn in it,
    /// so there is nothing to register beside it.
    fn publish(
        &mut self,
        _published: Rc<std::cell::RefCell<dyn tilo_session_core::door::Published>>,
    ) {
    }
}
