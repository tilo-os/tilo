//! `DragSource`: a view whose content a person can pick up and drag to another drop target in
//! the same window (a chat composer, a dock's run, a canvas).
//!
//! The toolkit already drags inside one window through the drag bus
//! ([`crate::widget::drag_bus`]): a run publishes what it lifted, every other run reads it, and
//! the release records the drop the run whose extent holds the pointer takes. What was missing
//! is a way for a *view* that is not a run's item — a search hit, a row of another view, a
//! message's attachment chip — to be the thing picked up. This widget is that: it wraps a
//! child, and a press that travels past [`DRAG_START_THRESHOLD_PX`] publishes the node's
//! `resource` on the bus with the node's `title` as the ghost's label.
//!
//! A press that does not travel is the child's own: the wrapper forwards it, so the row's
//! `activate`, the chip's `press` and every control inside keep working exactly as they would
//! without the wrapper.

use std::collections::HashMap;

use taffy::{NodeId, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::dnd::DRAG_START_THRESHOLD_PX;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::Rhythm;
use crate::widget::arrange::{Slot, Stack};
use crate::widget::{Decorator, Element, Widget};

/// What a `DragSource` carries on the drag bus: the kind its payload is written as, so a target
/// reads a resource id out of the blob without knowing which view made the drag.
pub const RESOURCE_PAYLOAD: &str = "resource";

#[path = "drag_source_component.rs"]
mod component;
pub use component::DragSourceProps;

/// A press that may become a drag.
#[derive(Clone, Copy, Debug)]
struct Lift {
    start: Point,
    /// The press travelled past the threshold: the drag is live on the bus.
    active: bool,
}

/// One view a person can drag out of, as a source on the drag bus. See the module docs.
pub struct DragSource {
    /// The child, in a stack of one: the wrapper's layout node is that stack's, so what it
    /// draws and lays out is the child whatever the child is.
    column: Stack,
    /// The resource the drag carries, as its address (`chat.workspace/messages/<id>`): what a
    /// drop target names when the pointer is let go.
    resource: String,
    /// What the ghost under the pointer says.
    title: String,
    /// The view's own key: what the bus knows this source by, so its payload never lands back
    /// on itself.
    key: String,
    rect: Rect,
    lift: Option<Lift>,
    /// Where the pointer last was, in this widget's logical coordinates.
    cursor: Point,
    dirty: bool,
}

impl DragSource {
    /// `child` dragged as the resource `resource`, shown under the pointer as `title`, known on
    /// the bus by `key`.
    pub fn new(
        key: impl Into<String>,
        child: Element,
        resource: impl Into<String>,
        title: impl Into<String>,
    ) -> Self {
        Self {
            column: Stack::stack()
                .fill_width()
                .gap(Rhythm::None)
                .push_slot(Slot::new(child)),
            resource: resource.into(),
            title: title.into(),
            key: key.into(),
            rect: Rect::ZERO,
            lift: None,
            cursor: Point::ZERO,
            dirty: true,
        }
    }

    /// What the drag carries.
    #[must_use]
    pub fn resource(&self) -> &str {
        &self.resource
    }

    /// The view the source wraps.
    #[must_use]
    pub fn content(&self) -> &dyn Widget {
        self.column
            .children()
            .into_iter()
            .next()
            .map_or_else(|| &self.column as &dyn Widget, |child| child)
    }

    /// Take new content, a resource and a title, as a rebuilt view gives them.
    pub(crate) fn set_content(
        &mut self,
        content: Option<Element>,
        resource: String,
        title: String,
    ) {
        if let Some(content) = content {
            let _ = self.column.replace(0, content);
        }
        if self.resource != resource || self.title != title {
            self.dirty = true;
        }
        self.resource = resource;
        self.title = title;
    }

    /// What the drag publishes: the resource's address in the blob.
    fn payload(&self) -> crate::dnd::DragPayload {
        crate::dnd::DragPayload::Custom {
            kind: RESOURCE_PAYLOAD.to_owned(),
            blob: self.resource.as_bytes().to_vec(),
        }
    }

    /// The press took the widget at `point`, when it is one of this source's to take: a drag
    /// may follow. Whether it did.
    fn press(&mut self, point: Point) -> bool {
        if !self.takes(point) {
            return false;
        }
        self.lift = Some(Lift {
            start: point,
            active: false,
        });
        self.dirty = true;
        true
    }

    /// Whether a press at `point` is this source's: over the view it wraps. A press anywhere
    /// else — on a bar laid over the windows the source stands in — is not the source's to
    /// take: taking it lifted a view the pointer was nowhere near and held the pointer, so the
    /// release that would have clicked what was pressed went to the source instead.
    fn takes(&self, point: Point) -> bool {
        self.column.get_rect().contains(point)
    }

    /// The pointer moved: past the threshold the drag is live on the bus, and stays live until
    /// it is let go.
    fn travel(&mut self, point: Point) {
        let Some(lift) = self.lift.as_mut() else {
            return;
        };
        if !lift.active && lift.start.distance(point) > DRAG_START_THRESHOLD_PX {
            lift.active = true;
        }
        if !lift.active {
            return;
        }
        self.dirty = true;
        crate::widget::drag_bus::publish(crate::widget::drag_bus::InFlight {
            source: self.key.clone(),
            payload: self.payload(),
            label: self.title.clone(),
            pointer: point,
            runs: vec![(self.key.clone(), self.rect)],
        });
    }

    /// Let go: the drop is recorded for whichever target's extent holds the pointer, and the
    /// press ends. `Some(true)` when a drag ran (the child sees no click), `Some(false)` when
    /// the press never travelled (the child may take the release as the click it is), `None`
    /// when there was no press here at all.
    fn release(&mut self) -> Option<bool> {
        let lift = self.lift.take()?;
        self.dirty = true;
        if lift.active {
            let landed = crate::widget::drag_bus::Landed {
                source: self.key.clone(),
                payload: self.payload(),
                pointer: self.cursor,
            };
            crate::widget::drag_bus::end(Some(landed));
        } else {
            crate::widget::drag_bus::end(None);
        }
        Some(lift.active)
    }
}

impl Decorator for DragSource {
    type Inner = Stack;

    fn inner(&self) -> &Stack {
        &self.column
    }

    fn inner_mut(&mut self) -> &mut Stack {
        &mut self.column
    }

    fn update(&mut self, state: &crate::widget::AppModel, path: &crate::widget::ModelPath) -> bool {
        let changed = self.column.update(state, path);
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
        self.dirty = false;
    }

    fn needs_redraw(&self) -> bool {
        self.dirty || self.column.needs_redraw()
    }

    fn wants_pointer_capture(&self) -> bool {
        self.lift.is_some()
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let point = ctx.logical(position);
                self.cursor = point;
                if self.lift.is_some() {
                    self.travel(point);
                    // A drag that started lets go of the child's press, so the release never
                    // also activates what was picked up.
                    if self.lift.is_some_and(|lift| lift.active) {
                        let away = WindowEvent::CursorLeft {
                            device_id: winit::event::DeviceId::dummy(),
                        };
                        self.column.handle_event(&away, ctx);
                        *ctx.handled = true;
                        return;
                    }
                }
            }
            WindowEvent::CursorLeft { .. } => {
                self.lift = None;
                self.dirty = true;
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                // The press is the child's first — a button in it takes it as any button does —
                // and it is the source's too: a press that travels lifts the whole view, and one
                // that does not is the child's click, taken on the release below.
                self.column.handle_event(event, ctx);
                // The source is the view it wraps, and only that view: a press elsewhere is not
                // its to lift, nor to hold the pointer for.
                if self.press(self.cursor) {
                    *ctx.focus_target = Some(ctx.node_id);
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } if self.lift.is_some() => {
                if self.release() == Some(true) {
                    *ctx.handled = true;
                    return;
                }
            }
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed
                    && event.logical_key == Key::Named(NamedKey::Escape)
                    && self.lift.is_some() =>
            {
                self.lift = None;
                self.dirty = true;
                crate::widget::drag_bus::end(None);
                *ctx.handled = true;
                return;
            }
            _ => {}
        }
        self.column.handle_event(event, ctx);
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        self.column.cursor_shape(point)
    }

    fn get_rect(&self) -> Rect {
        self.column.get_rect()
    }

    fn name(&self) -> &'static str {
        "DragSource"
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

#[cfg(test)]
#[path = "drag_source_tests.rs"]
mod tests;

impl crate::component::SlotViews for DragSource {
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
