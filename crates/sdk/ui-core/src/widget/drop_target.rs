//! `DropTarget`: a view that takes what another view dragged ([`super::DragSource`] or a run's
//! item) and reports it as one event.
//!
//! A drop target reads the drag bus ([`crate::widget::drag_bus`]): while a drag is in flight it
//! shows the toolkit's drop line over itself, and when the drag is let go inside its box it
//! fires `drop` with the resource the drag carried. Nothing is taken that the target's view did
//! not ask for: a drag let go elsewhere is left for whatever target does hold the pointer.

use std::collections::HashMap;

use taffy::{NodeId, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use winit::event::WindowEvent;

use crate::component::Event;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::Rhythm;
use crate::widget::arrange::{Slot, Stack};
use crate::widget::{Decorator, Element, Widget};

/// The resource a payload names, when it names one: a
/// [resource](super::drag_source::RESOURCE_PAYLOAD) drag's id, or a run item's
/// ([`ITEM_PAYLOAD`](super::drag_run::ITEM_PAYLOAD): its key is the resource it stands for's id).
fn resource_of(payload: &crate::dnd::DragPayload) -> Option<String> {
    let crate::dnd::DragPayload::Custom { kind, blob } = payload else {
        return None;
    };
    if kind != super::drag_source::RESOURCE_PAYLOAD && kind != super::drag_run::ITEM_PAYLOAD {
        return None;
    }
    String::from_utf8(blob.clone())
        .ok()
        .filter(|id| !id.is_empty())
}

#[path = "drop_target_component.rs"]
mod component;
pub use component::DropTargetProps;

/// A view that takes drags. See the module docs.
pub struct DropTarget {
    /// The child, in a stack of one: the target's layout node is that stack's, so what it draws
    /// and lays out is the child whatever the child is.
    column: Stack,
    /// The resource a drag over it would drop now, while one hovers: what the line says.
    hovered: Option<String>,
    /// What a drop reports.
    drop: Event,
    /// The key this target is known by on the bus.
    key: String,
    rect: Rect,
    dirty: bool,
}

impl DropTarget {
    /// `child`, taking drops, known on the bus by `key` and reporting them through `drop`.
    pub fn new(key: impl Into<String>, child: Element, drop: Event) -> Self {
        Self {
            column: Stack::stack()
                .fill_width()
                .gap(Rhythm::None)
                .push_slot(Slot::new(child)),
            hovered: None,
            drop,
            key: key.into(),
            rect: Rect::ZERO,
            dirty: true,
        }
    }

    /// Take new content and what a drop reports, as a rebuilt view gives them.
    pub(crate) fn set_content(&mut self, content: Option<Element>, drop: Event) {
        if let Some(content) = content {
            let _ = self.column.replace(0, content);
        }
        self.drop = drop;
    }

    /// Where a drop may land: this widget's own box.
    #[must_use]
    pub fn extent(&self) -> Rect {
        self.rect
    }

    /// Take the drop the last drag left inside this target, if any: the resource it carried.
    /// One target takes it; the rest see nothing.
    fn take(&mut self) -> Option<String> {
        let landed = crate::widget::drag_bus::take(&self.key, self.rect)?;
        let resource = resource_of(&landed.payload)?;
        self.dirty = true;
        Some(resource)
    }

    /// The resource a drag over the target now carries, while one hovers it.
    fn over_now(&self) -> Option<String> {
        let in_flight = crate::widget::drag_bus::current()?;
        if in_flight.source == self.key || !self.rect.contains(in_flight.pointer) {
            return None;
        }
        resource_of(&in_flight.payload)
    }
}

impl Decorator for DropTarget {
    type Inner = Stack;

    fn inner(&self) -> &Stack {
        &self.column
    }

    fn inner_mut(&mut self) -> &mut Stack {
        &mut self.column
    }

    fn update(&mut self, state: &crate::widget::AppModel, path: &crate::widget::ModelPath) -> bool {
        let mut changed = self.column.update(state, path);
        // A drop let go inside this target since the last pass is the event it fires; a drag
        // that came or went changes the line it draws.
        if let Some(resource) = self.take() {
            self.drop
                .fire(Some(crate::component::Value::Resource(resource)));
            changed = true;
        }
        let over = self.over_now();
        if over != self.hovered {
            self.hovered = over;
            self.dirty = true;
            changed = true;
        }
        changed | self.dirty
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
        self.column.apply_layout(taffy, node, origin);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.column.draw(scene, transform, text, theme, presence);
        if self.hovered.is_some() && self.rect.width() > 0.0 {
            // The same line every drop indicator draws: your colour on the edge.
            let line = f64::from(theme.tokens.metrics.drag_line_px).max(1.0);
            let radius = f64::from(theme.tokens.radii.control);
            let shape = RoundedRect::from_rect(self.rect.inflate(-line / 2.0, -line / 2.0), radius);
            scene.stroke(
                &Stroke::new(line),
                transform,
                theme.tokens.colors.intent.accent,
                None,
                &shape,
            );
        }
        self.dirty = false;
    }

    fn needs_redraw(&self) -> bool {
        self.dirty || self.column.needs_redraw()
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        self.column.handle_event(event, ctx);
    }

    fn get_rect(&self) -> Rect {
        self.column.get_rect()
    }

    fn name(&self) -> &'static str {
        "DropTarget"
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.column.build_layout(taffy, theme, node_map)
    }

    fn for_each_child(&self, visit: &mut dyn FnMut(&dyn Widget)) {
        self.column.for_each_child(visit);
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

impl crate::component::SlotViews for DropTarget {
    fn len(&self) -> usize {
        1
    }

    fn view_mut(&mut self, index: usize) -> Option<&mut Element> {
        (index == 0).then_some(())?;
        self.column.child_mut(0)
    }

    fn insert(&mut self, index: usize, view: Element) {
        self.column.insert(index, Slot::new(view));
    }

    fn remove(&mut self, index: usize) {
        self.column.remove(index);
    }

    fn move_view(&mut self, from: usize, to: usize) {
        self.column.move_slot(from, to);
    }

    fn replace(&mut self, index: usize, view: Element) {
        let _ = self.column.replace(index, view);
    }
}
