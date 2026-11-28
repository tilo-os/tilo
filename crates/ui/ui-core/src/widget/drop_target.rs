//! `DropTarget`: a view that takes what another view dragged ([`super::DragSource`] or a run's
//! item) and reports it as one event.
//!
//! A drop target reads the drag bus ([`crate::widget::drag_bus`]): while a drag is in flight it
//! shows the toolkit's drop line over itself, and when the drag is let go inside its box it
//! fires `drop` with the resource the drag carried and the point inside it the person let go
//! at. Nothing is taken that the target's view did not ask for: a drag let go elsewhere is left
//! for whatever target does hold the pointer.

use std::collections::HashMap;

use crate::time::{Duration, Instant};
use taffy::{NodeId, TaffyTree};
use vello::kurbo::{Point, Rect, RoundedRect, Stroke};
use winit::event::WindowEvent;

use tilo_ui_base::component::Event;

use crate::widget::arrange::{Part, Stack};
use tilo_ui_base::theme::Theme;
use tilo_ui_base::tokens::Rhythm;
use tilo_ui_base::{widget::Decorator, widget::DrawChild, widget::Element, widget::Widget};

/// What a drop carries a target's view: a resource, or device files a source of this window stood
/// for.
enum Dropped {
    Resource(String),
    Files(Vec<String>),
}

/// How long a drag rests on a target before the target says so: enough for a pointer carrying an
/// entry across a row to pass, short enough that a folder opens under a person's hand. A target
/// that opens a folder after it is what a file manager's spring-loaded folders are.
pub const SPRING: Duration = Duration::from_millis(700);

/// What a payload carries, when it is something a target takes: a
/// [resource](super::drag_source::RESOURCE_PAYLOAD) drag's id, a run item's
/// ([`ITEM_PAYLOAD`](super::drag_run::ITEM_PAYLOAD): its key is the resource it stands for's id),
/// or the paths of a [files](super::drag_source::FILES_PAYLOAD) drag.
fn dropped_of(payload: &tilo_ui_base::dnd::DragPayload) -> Option<Dropped> {
    let tilo_ui_base::dnd::DragPayload::Custom { kind, blob } = payload else {
        return None;
    };
    if kind == super::drag_source::FILES_PAYLOAD {
        let files = super::drag_source::files_of(blob);
        return (!files.is_empty()).then_some(Dropped::Files(files));
    }
    if kind != super::drag_source::RESOURCE_PAYLOAD && kind != super::drag_run::ITEM_PAYLOAD {
        return None;
    }
    String::from_utf8(blob.clone())
        .ok()
        .filter(|id| !id.is_empty())
        .map(Dropped::Resource)
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
    /// What a drop of a resource reports.
    drop: Event,
    /// What a drop of a source's device files reports.
    drop_files: Event,
    /// What a drag that rests on the target reports, once, while it still holds it: a folder's
    /// row opens its folder from here.
    spring: Event,
    /// When the drag that is over the target now has rested on it long enough to be said
    /// (`SPRING`), while one is.
    spring_at: Option<Instant>,
    /// The key this target is known by on the bus.
    key: String,
    rect: Rect,
    /// Where the pointer is, in the tree's own pixels: where a drag the desktop carries is.
    cursor: Point,
    /// Whether the desktop carries files over the window now.
    files_over: bool,
    dirty: bool,
}

impl DropTarget {
    /// `child`, taking drops, known on the bus by `key` and reporting them through `drop`.
    pub fn new(key: impl Into<String>, child: Element, drop: Event) -> Self {
        Self {
            // The room the part gives it is the target's: a gap between windows takes the row's
            // whole height, so a tab carried into it at any height lands, where the bare content
            // it holds would give it only as much as that content measures.
            column: Stack::stack()
                .fill_width()
                .fill_height()
                .gap(Rhythm::None)
                .push_part(Part::new(child)),
            hovered: None,
            drop,
            drop_files: Event::default(),
            spring: Event::default(),
            spring_at: None,
            key: key.into(),
            rect: Rect::ZERO,
            cursor: Point::ZERO,
            files_over: false,
            dirty: true,
        }
    }

    /// What a drop of device files a source of this window stood for reports: their paths.
    #[must_use]
    pub fn with_files(mut self, drop_files: Event) -> Self {
        self.drop_files = drop_files;
        self
    }

    /// What a drag that has rested on the target for [`SPRING`] reports, once per drag: a folder
    /// whose row opens under the hand carrying a file or another entry.
    #[must_use]
    pub fn with_spring(mut self, spring: Event) -> Self {
        self.spring = spring;
        self
    }

    /// Take new content and what a drop reports, as a rebuilt view gives them.
    pub(crate) fn set_content(&mut self, content: Option<Element>, drop: Event, drop_files: Event) {
        if let Some(content) = content {
            let _ = self.column.replace(0, content);
        }
        self.drop = drop;
        self.drop_files = drop_files;
    }

    /// Take what a drag that rests on the target reports, as a rebuilt view gives it.
    pub(crate) fn set_spring(&mut self, spring: Event) {
        self.spring = spring;
    }

    /// Where a drop may land: this widget's own box.
    #[must_use]
    pub fn extent(&self) -> Rect {
        self.rect
    }

    /// Take the drop the last drag left inside this target, if any: the resource it carried and
    /// where inside the target it was let go, in the target's own logical pixels. One target
    /// takes it; the rest see nothing.
    fn take(&mut self) -> Option<(Dropped, Point)> {
        let landed = crate::widget::drag_bus::take(&self.key, self.rect)?;
        let dropped = dropped_of(&landed.payload)?;
        self.spring_at = None;
        self.dirty = true;
        Some((dropped, landed.pointer - self.rect.origin().to_vec2()))
    }

    /// The resource a drag over the target now carries, while one hovers it: one on the bus, or
    /// one whose files a source of this window handed to the desktop.
    fn over_now(&self) -> Option<String> {
        if self.files_over && self.rect.contains(self.cursor) {
            return crate::widget::drag_bus::offered();
        }
        let in_flight = crate::widget::drag_bus::current()?;
        if in_flight.source == self.key || !self.rect.contains(in_flight.pointer) {
            return None;
        }
        dropped_of(&in_flight.payload).map(|_| in_flight.label)
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

    fn update(
        &mut self,
        state: &tilo_ui_base::widget::AppModel,
        path: &tilo_ui_base::widget::ModelPath,
    ) -> bool {
        let mut changed = self.column.update(state, path);
        // A drop let go inside this target since the last pass is the event it fires; a drag
        // that came or went changes the line it draws.
        if let Some((dropped, at)) = self.take() {
            let at = tilo_ui_base::component::Value::List(vec![
                tilo_ui_base::component::Value::Int(at.x.round() as i64),
                tilo_ui_base::component::Value::Int(at.y.round() as i64),
            ]);
            match dropped {
                Dropped::Resource(resource) => {
                    self.drop
                        .fire(Some(tilo_ui_base::component::Value::List(vec![
                            tilo_ui_base::component::Value::Resource(resource),
                            at,
                        ])));
                }
                Dropped::Files(files) => {
                    self.drop_files
                        .fire(Some(tilo_ui_base::component::Value::List(vec![
                            tilo_ui_base::component::Value::List(
                                files
                                    .into_iter()
                                    .map(tilo_ui_base::component::Value::Text)
                                    .collect(),
                            ),
                            at,
                        ])));
                }
            }
            changed = true;
        }
        let over = self.over_now();
        if over != self.hovered {
            self.hovered = over;
            // A drag that has just come over the target rests on it from now: the moment it has
            // rested long enough is the deadline the loop wakes the target at. A drag that has
            // gone takes the deadline with it.
            self.spring_at = self.hovered.as_ref().map(|_| crate::time::now() + SPRING);
            self.dirty = true;
            changed = true;
        }
        changed | self.dirty
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = tilo_ui_base::widget::layout_rect(taffy, node, origin);
        self.column.apply_layout(taffy, node, origin);
    }

    fn draw(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text, theme, mut env) = ctx.parts();
        self.column.draw_child(scene, transform, text, theme, &env);
        if let Some(label) = self.hovered.as_deref().filter(|label| !label.is_empty())
            && self.rect.width() > 0.0
        {
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
            // What the drag carries follows the pointer, over the target: the person sees the tab
            // or the window they hold where it would land, under their hand.
            let pointer = crate::widget::drag_bus::current()
                .map_or(self.cursor, |in_flight| in_flight.pointer);
            let size = 12.0 * theme.text_scale;
            let (said, said_height) = text.measure(label, size, None);
            let pad = 8.0;
            let width = f64::from(said) + 2.0 * pad;
            let height = f64::from(said_height) + 2.0 * pad;
            // Before the pointer: a target drawn after this one (the window after a gap) covers
            // what this draws, so the card stands over the ground already drawn.
            let x1 = (pointer.x - 14.0).max(width + 8.0);
            let y0 = pointer.y + 18.0;
            let chip = Rect::new(x1 - width, y0, x1, y0 + height);
            let ground = RoundedRect::from_rect(chip, radius);
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                theme.tokens.colors.surfaces.overlay,
                None,
                &ground,
            );
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                theme.tokens.colors.intent.accent,
                None,
                &ground,
            );
            let budget = tilo_ui_base::num::f32_of_f64((width - 2.0 * pad).max(0.0));
            let said = text.elide_to_width(label, size, budget);
            text.draw_text(
                scene,
                &said,
                size,
                theme.tokens.colors.text_primary,
                transform.pre_translate(vello::kurbo::Vec2::new(chip.x0 + pad, chip.y0 + pad)),
                None,
            );
        }
        self.dirty = false;
    }

    fn needs_redraw(&self) -> bool {
        self.dirty || self.column.needs_redraw()
    }

    fn next_wake_deadline(&self) -> Option<Instant> {
        let inner = self.column.next_wake_deadline();
        match (self.spring_at, inner) {
            (Some(rested), Some(inner)) => Some(rested.min(inner)),
            (rested, inner) => rested.or(inner),
        }
    }

    fn serve_deadline(&mut self, now: Instant) -> bool {
        let mut changed = self.column.serve_deadline(now);
        if let Some(rested) = self.spring_at
            && now >= rested
        {
            self.spring_at = None;
            // The drag may have ended between the deadline and this pass: only a drag the target
            // holds now has rested on it.
            if self.over_now().is_some() {
                self.spring.fire(None);
                changed = true;
            }
        }
        changed
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut tilo_ui_base::widget::EventCtx<'_>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = ctx.logical(position);
                self.dirty |= self.files_over;
            }
            WindowEvent::HoveredFile(_) => {
                self.files_over = true;
                self.dirty = true;
            }
            WindowEvent::HoveredFileCancelled => {
                self.files_over = false;
                self.dirty = true;
            }
            _ => {}
        }
        self.column.handle_event(event, ctx);
        // Files a source of this window gave the desktop to carry, dropped back inside this
        // target, are that source's drag landing here: taken as a drop of the bus, at the point
        // the person let go. A target inside this one took it first when it held the pointer.
        if let WindowEvent::DroppedFile(path) = event {
            self.files_over = false;
            if !*ctx.handled
                && self.rect.contains(self.cursor)
                && crate::widget::drag_bus::came_back(path, self.cursor)
            {
                self.dirty = true;
                *ctx.handled = true;
            }
        }
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

impl tilo_ui_base::component::PartViews for DropTarget {
    fn len(&self) -> usize {
        1
    }

    fn view_mut(&mut self, index: usize) -> Option<&mut Element> {
        (index == 0).then_some(())?;
        self.column.child_mut(0)
    }

    fn insert(&mut self, index: usize, view: Element) {
        self.column.insert(index, Part::new(view));
    }

    fn remove(&mut self, index: usize) {
        self.column.remove(index);
    }

    fn move_view(&mut self, from: usize, to: usize) {
        self.column.move_part(from, to);
    }

    fn replace(&mut self, index: usize, view: Element) {
        let _ = self.column.replace(index, view);
    }
}

#[cfg(test)]
#[path = "drop_target_tests.rs"]
mod tests;
