//! One part of a view, chosen by view-local state.

use std::collections::HashMap;
use std::rc::Rc;
use ui_core::widget::{AppModel, ModelPath};

use taffy::{NodeId, TaffyTree};
use ui_core::automerge::ObjId;
use ui_core::presence::PresenceCtx;
use ui_core::text_render::TextRenderer;
use ui_core::vello::Scene;
use ui_core::vello::kurbo::{Affine, Point, Rect};
use ui_core::widget::{Element, Widget};
use ui_core::winit::event::WindowEvent;
use ui_core::{LocalState, Theme, taffy};

type BuildPart<K> = Box<dyn Fn(&K, &Rc<()>) -> Element>;

/// The part [`ViewCx::switch`](super::ViewCx::switch) shows for the current
/// key. Rebuilt when the key changes; dropping the old part retires every
/// binding it registered.
pub(crate) struct Switch<K> {
    key: LocalState<K>,
    shown: K,
    build: BuildPart<K>,
    /// Dropping it retires every binding the shown part registered.
    alive: Rc<()>,
    part: Element,
    structure_dirty: bool,
}

impl<K: Clone + PartialEq + 'static> Switch<K> {
    pub(crate) fn new(key: LocalState<K>, build: BuildPart<K>) -> Self {
        let shown = key.with(Clone::clone);
        let alive = Rc::new(());
        let part = build(&shown, &alive);
        Self {
            key,
            shown,
            build,
            alive,
            part,
            structure_dirty: false,
        }
    }

    fn reconcile(&mut self) {
        let current = self.key.with(Clone::clone);
        if current == self.shown {
            return;
        }
        let alive = Rc::new(());
        self.part = (self.build)(&current, &alive);
        self.alive = alive;
        self.shown = current;
        self.structure_dirty = true;
    }
}

impl<K: Clone + PartialEq + 'static> Widget for Switch<K> {
    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.reconcile();
        self.part.sync(state, path);
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        self.part.update(state, path)
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.part.build_layout(taffy, theme, node_map)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.part.apply_layout(taffy, node, origin);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.part.draw(scene, transform, text, theme, presence);
    }

    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        permissions: &ui_core::permissions::PermissionContext,
    ) {
        self.part
            .draw_with_presence(scene, transform, text, theme, presence, permissions);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui_core::widget::EventCtx<'_>) {
        self.part.handle_event(event, ctx);
    }

    fn wants_pointer_capture(&self) -> bool {
        self.part.wants_pointer_capture()
    }

    fn take_structure_dirty(&mut self) -> bool {
        // The key may change between syncs (a selection made in this very
        // event), so the structure is re-checked here as well.
        self.reconcile();
        let own = std::mem::take(&mut self.structure_dirty);
        self.part.take_structure_dirty() | own
    }

    fn start_operation(
        &mut self,
        op: &ui_core::host_operation::ResourceOperation<'_>,
        doc: &mut ui_core::widget::Edits<'_>,
        path: &ObjId,
    ) -> Result<Option<ui_core::host_operation::ResourceOperationStart>, String> {
        ui_core::host_operation::start_in_children(self, op, doc, path)
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![self.part.as_ref()]
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![self.part.as_mut()]
    }

    fn name(&self) -> &'static str {
        "Switch"
    }

    fn get_rect(&self) -> Rect {
        self.part.get_rect()
    }
}
