//! `DragRun`: a run of items — a dock's icons, a toolbar's tiles, a row of
//! cards — in which an item can be picked up and moved to another place, and
//! a resource from elsewhere (an app, a file, a search result) can be dropped
//! in.
//!
//! The gesture is Rust and runs at native refresh: a press starts a candidate,
//! pointer travel past the drag threshold lifts the item into a ghost, and the
//! place it would land is shown before it is let go, with an insertion line
//! across the run. Releasing reports **one** [`DragDrop`]: the key of the item
//! moved and the item it goes before, or the resource that was dropped and its
//! place. The owner turns that into one action on its model
//! (`items.move(item, before: next)`), so a reorder is attributed, permitted
//! and undone like every other change; the run then shows the item where the
//! model puts it.
//!
//! A host whose run takes drops that start in another widget (a resource
//! dragged from a search result onto a dock) says what is being dragged with
//! [`DragRun::set_foreign`]; the run then shows the same insertion line, and
//! [`DragRun::take_foreign`] answers where it would land when the host's drag
//! ends.
//!
//! A touchscreen holds an item to lift it and then drags; a drag that begins
//! without the hold scrolls what the run is in. Escape puts a lifted item down
//! where it was.

use std::collections::HashMap;

use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::Fill;
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::dnd::{DRAG_START_THRESHOLD_PX, DragPayload};
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::{Column, Element, Row, TaffyTreeExt, Widget};

/// Which way a run goes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RunAxis {
    /// Leading to trailing: a dock, a toolbar.
    #[default]
    Horizontal,
    /// Top to bottom: a column of items.
    Vertical,
}

crate::component::choice!(RunAxis, "Axis", { Horizontal => "horizontal", Vertical => "vertical" });

impl RunAxis {
    /// The pointer's coordinate along this axis.
    fn along(self, point: Point) -> f64 {
        match self {
            Self::Horizontal => point.x,
            Self::Vertical => point.y,
        }
    }
}

/// Where a dropped thing goes: the run that took it, what moved, and the
/// place it lands at, counted from the leading edge among the run's other
/// items.
///
/// A place, not a neighbour: that is what a collection's `move(x, to: i)`
/// takes, and it is resolved to the item it then comes before when the action
/// runs, so concurrent moves converge where each person meant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DragDrop {
    /// The key of the run that took the drop.
    pub run: String,
    /// What was dropped: the key of the item moved, or the resource dragged in.
    pub moved: String,
    /// The place it lands at among the run's other items, from 0.
    pub at: i64,
    /// The key of the item it lands before; `None` for the end of the run.
    pub before: Option<String>,
}

/// One item of a run.
pub struct RunItem {
    /// The stable key the item is known by, and what the run reports moving.
    pub key: String,
    /// The resource the item stands for, when it is one: what a drop reports.
    pub item: Option<crate::component::ResourceId>,
    /// The item's own label, for the ghost.
    pub label: String,
    /// The item's view.
    pub view: Element,
}

impl std::fmt::Debug for RunItem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunItem")
            .field("key", &self.key)
            .field("label", &self.label)
            .finish_non_exhaustive()
    }
}

/// The keys and ghost content of one item, in the order the model gave them.
struct Keys {
    key: String,
    label: String,
    /// The resource the item stands for, when it is one.
    item: Option<crate::component::ResourceId>,
}

/// A lifted item, from press to release.
struct Lift {
    key: String,
    /// The item's rect when it was lifted, and where in it the pointer took
    /// it.
    rect: Rect,
    grab: Point,
    /// The pointer's place when the press landed, for the drag threshold.
    start: Point,
    /// Whether it has travelled far enough to be a drag rather than a click.
    active: bool,
}

/// What a drag over the run would land on: the item it goes before, and the
/// line drawn there.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Landing {
    /// The place among the run's other items.
    at: i64,
    before: Option<String>,
}

/// See the [module docs](self).
pub struct DragRun {
    id: String,
    name: Option<String>,
    axis: RunAxis,
    /// The items' views, in order, built into [`Self::items`] at the first
    /// layout.
    views: Vec<Element>,
    /// The items, in order: a `Row` or a `Column` by the axis.
    items: Option<Box<dyn Widget>>,
    keys: Vec<Keys>,
    lift: Option<Lift>,
    cursor: Point,
    /// What another widget is dragging over the run, and its ghost's label.
    foreign: Option<(DragPayload, String)>,
    /// Where a foreign drag would land, while it is over the run.
    landing: Option<Landing>,
    /// What the run dropped since the host last read it, and what to tell the
    /// owner of each, when the run was built from a described component.
    drops: Vec<DragDrop>,
    reporter: Option<Box<dyn FnMut(&DragDrop)>>,
    /// Called with the key of an item pressed and let go without moving.
    activated: Option<Box<dyn FnMut(&str)>>,
    dirty: bool,
}

impl DragRun {
    /// A run `id` (its test id), its items leading to trailing.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: None,
            axis: RunAxis::Horizontal,
            views: Vec::new(),
            items: None,
            keys: Vec::new(),
            lift: None,
            cursor: Point::ZERO,
            foreign: None,
            landing: None,
            drops: Vec::new(),
            reporter: None,
            activated: None,
            dirty: false,
        }
    }

    /// A run that goes `axis`.
    #[must_use]
    pub fn axis(mut self, axis: RunAxis) -> Self {
        self.axis = axis;
        self.items = None;
        self
    }

    /// What the run is, for assistive technology.
    #[must_use]
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Each item's key and the resource it stands for, when it is one.
    #[must_use]
    pub fn item_ids(&self) -> Vec<(String, Option<crate::component::ResourceId>)> {
        self.keys
            .iter()
            .map(|keys| (keys.key.clone(), keys.item.clone()))
            .collect()
    }

    /// Add `item` at the end of the run.
    #[must_use]
    pub fn push(mut self, item: RunItem) -> Self {
        self.keys.push(Keys {
            key: item.key,
            label: item.label,
            item: item.item,
        });
        self.views.push(item.view);
        self
    }

    /// The run's key, as a drop names it.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Whether an item is lifted now.
    #[must_use]
    pub fn is_lifting(&self) -> bool {
        self.lift.as_ref().is_some_and(|lift| lift.active)
    }

    /// What another widget is dragging over this run: `payload`, shown in the
    /// ghost as `label`. The run then shows where it would land, and
    /// [`Self::take_foreign`] answers it when the drag ends.
    pub fn set_foreign(&mut self, foreign: Option<(DragPayload, String)>) {
        if self
            .foreign
            .as_ref()
            .map(|(p, l)| (format!("{p:?}"), l.clone()))
            != foreign.as_ref().map(|(p, l)| (format!("{p:?}"), l.clone()))
        {
            self.foreign = foreign;
            self.landing = None;
            self.dirty = true;
        }
    }

    /// Whether a foreign drag is over the run now.
    #[must_use]
    pub fn foreign_is_over(&self) -> bool {
        self.landing.is_some()
    }

    /// Where the foreign drag over the run would land, once its host ends it:
    /// the resource and the item it goes before.
    #[must_use]
    pub fn take_foreign(&mut self) -> Option<DragDrop> {
        let (payload, _) = self.foreign.take()?;
        let landing = self.landing.take()?;
        self.dirty = true;
        let DragPayload::Custom { kind, blob } = payload else {
            return None;
        };
        if kind != ITEM_PAYLOAD {
            return None;
        }
        Some(DragDrop {
            run: self.id.clone(),
            moved: String::from_utf8(blob).ok()?,
            at: landing.at,
            before: landing.before,
        })
    }

    /// What the run dropped since the host last read it: an item moved, or a
    /// resource dropped on it.
    pub fn take_drops(&mut self) -> Vec<DragDrop> {
        std::mem::take(&mut self.drops)
    }

    /// Each drop goes to `report`, as a described component's event: a host
    /// that keeps its own drops instead reads them with [`Self::take_drops`].
    #[must_use]
    pub fn on_drop(mut self, report: impl FnMut(&DragDrop) + 'static) -> Self {
        self.reporter = Some(Box::new(report));
        self
    }

    /// Each item pressed and let go without moving goes to `activated`, by its
    /// key: the item's view does what its press does.
    #[must_use]
    pub fn on_activate(mut self, activated: impl FnMut(&str) + 'static) -> Self {
        self.activated = Some(Box::new(activated));
        self
    }

    /// Send each drop to the reporter, if it has one.
    fn report(&mut self) {
        let Some(mut reporter) = self.reporter.take() else {
            return;
        };
        for drop in std::mem::take(&mut self.drops) {
            reporter(&drop);
        }
        self.reporter = Some(reporter);
    }

    /// What a lifted item hands another widget when the pointer leaves the
    /// run, carrying the item's key.
    fn payload_of(&self, key: &str) -> DragPayload {
        DragPayload::Custom {
            kind: ITEM_PAYLOAD.to_owned(),
            blob: key.as_bytes().to_vec(),
        }
    }

    /// The container this run's items go in, by its axis.
    fn build_items(&self, views: Vec<Element>) -> Box<dyn Widget> {
        match self.axis {
            RunAxis::Horizontal => {
                // A run takes a second line rather than pressing its items into each other:
                // a bar too narrow for its parts wraps, as the bar itself does, and a dock
                // whose icons do not fit wraps in place. The run still reports places in the
                // model's order, so a wrapped run's places stay the ones the owner's action
                // takes.
                let mut row = Row::new().align_start();
                for view in views {
                    // Each item is its own no-wrap row: a row's `flex_shrink` is 0, so an
                    // item keeps the width its view takes and the run wraps whole items onto
                    // the next line rather than squeezing one below its hit target.
                    row = row.push(Box::new(Row::new().no_wrap().align_start().push(view)));
                }
                Box::new(row)
            }
            RunAxis::Vertical => {
                let mut column = Column::new().with_no_padding();
                for view in views {
                    column = column.push(view);
                }
                Box::new(column)
            }
        }
    }

    /// Each item's rect, in order.
    fn item_rects(&self) -> Vec<Rect> {
        self.items
            .as_ref()
            .map(|items| {
                items
                    .children()
                    .iter()
                    .map(|child| child.get_rect())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The run's whole extent, for a drop to land in.
    #[must_use]
    pub fn extent(&self) -> Rect {
        self.get_rect()
    }

    /// The run's items' extent, from their rects.
    fn band(&self) -> Option<Rect> {
        self.item_rects()
            .into_iter()
            .reduce(|band, rect| band.union(rect))
    }

    /// The item under `point`, by its place in the run.
    fn item_at(&self, point: Point) -> Option<usize> {
        self.item_rects()
            .into_iter()
            .position(|rect| rect.contains(point))
    }

    /// The item a pointer at `point` lands before, or `None` when it is off
    /// the run.
    ///
    /// Measured against each item's middle, so a half-item is the dead zone
    /// every reorder uses: the line only moves once the pointer crosses a
    /// neighbour's middle. One item's reach past either end lets an item be
    /// dropped just outside the run, which is where a dock's last place is.
    fn target_at(&self, point: Point, lifted: Option<&str>) -> Option<Landing> {
        let rects = self.item_rects();
        let band = self.band()?;
        let pitch = match self.axis {
            RunAxis::Horizontal => band.width(),
            RunAxis::Vertical => band.height(),
        } / rects.len().max(1) as f64;
        let reach = rects.first().map_or(0.0, |rect| match self.axis {
            RunAxis::Horizontal => rect.width(),
            RunAxis::Vertical => rect.height(),
        });
        let band = match self.axis {
            RunAxis::Horizontal => Rect::new(band.x0, band.y0, band.x1 + pitch.min(reach), band.y1),
            RunAxis::Vertical => Rect::new(band.x0, band.y0, band.x1, band.y1 + pitch.min(reach)),
        };
        if !band.contains(point) {
            return None;
        }
        let along = self.axis.along(point);
        let mut at = 0i64;
        let mut before = None;
        for (rect, keys) in rects.iter().zip(&self.keys) {
            if Some(keys.key.as_str()) == lifted {
                continue;
            }
            if along < self.axis.along(rect.center()) {
                before = Some(keys.key.clone());
                break;
            }
            at += 1;
        }
        Some(Landing { at, before })
    }

    /// The insertion line for `landing`, across the run.
    fn line_rect(&self, landing: &Landing, theme: &Theme) -> Option<Rect> {
        let gap = f64::from(theme.tokens.spacing.s2);
        let line = f64::from(theme.tokens.metrics.drag_line_px).max(1.0);
        let rects = self.item_rects();
        let band = self.band()?;
        let at = match &landing.before {
            Some(key) => self.keys.iter().position(|keys| &keys.key == key)?,
            None => rects.len(),
        };
        Some(match self.axis {
            RunAxis::Horizontal => {
                let x = rects.get(at).map_or_else(
                    || rects.last().map_or(band.x0, |rect| rect.x1) + gap / 2.0,
                    |r| r.x0 - gap / 2.0,
                );
                Rect::new(x - line / 2.0, band.y0, x + line / 2.0, band.y1)
            }
            RunAxis::Vertical => {
                let y = rects.get(at).map_or_else(
                    || rects.last().map_or(band.y0, |rect| rect.y1) + gap / 2.0,
                    |r| r.y0 - gap / 2.0,
                );
                Rect::new(band.x0, y - line / 2.0, band.x1, y + line / 2.0)
            }
        })
    }

    /// Take the press at `point`: lift the item under it, if there is one.
    fn press(&mut self, point: Point) -> bool {
        let Some(at) = self.item_at(point) else {
            return false;
        };
        let (Some(keys), Some(rect)) = (self.keys.get(at), self.item_rects().get(at).copied())
        else {
            return false;
        };
        self.lift = Some(Lift {
            key: keys.key.clone(),
            rect,
            grab: Point::new(point.x - rect.x0, point.y - rect.y0),
            start: point,
            active: false,
        });
        self.dirty = true;
        true
    }

    /// Follow the pointer while something is lifted: past the threshold the
    /// drag starts, and the landing it would take moves with it.
    fn travel(&mut self, point: Point) {
        self.cursor = point;
        let Some(lift) = self.lift.as_mut() else {
            return;
        };
        if !lift.active && lift.start.distance(point) > DRAG_START_THRESHOLD_PX {
            lift.active = true;
        }
        if !lift.active {
            return;
        }
        let key = lift.key.clone();
        let label = self
            .keys
            .iter()
            .find(|keys| keys.key == key)
            .map_or_else(String::new, |keys| keys.label.clone());
        self.landing = self.target_at(point, Some(&key));
        self.dirty = true;
        // Every other run in the window may take this item now, and this one
        // shows where it lands.
        crate::widget::drag_bus::publish(crate::widget::drag_bus::InFlight {
            source: self.id.clone(),
            payload: self.payload_of(&key),
            label,
            pointer: point,
            runs: vec![(self.id.clone(), self.extent())],
        });
    }

    /// Put a lifted item down where it was.
    fn cancel(&mut self) {
        if self.is_lifting() {
            crate::widget::drag_bus::end(None);
        }
        self.lift = None;
        self.landing = None;
        self.dirty = true;
    }

    /// Let go of a lifted item: report the one move it makes, if it makes one, and whether the
    /// press was only a click — the item's own view took it, so nothing moves.
    fn release(&mut self) -> (Option<DragDrop>, bool) {
        let Some(lift) = self.lift.take() else {
            return (None, false);
        };
        let landing = self.landing.take();
        self.dirty = true;
        if !lift.active {
            // A press that never moved is a click: the item's own view takes the release, and
            // nothing moves.
            return (None, true);
        }
        // Let go: another run whose extent holds the pointer takes it, or
        // nothing does.
        let landed = crate::widget::drag_bus::Landed {
            source: self.id.clone(),
            payload: self.payload_of(&lift.key),
            pointer: self.cursor,
        };
        let Some(landing) = landing else {
            // Let go outside this run: another run whose extent holds the
            // pointer takes it, or nothing does.
            crate::widget::drag_bus::end(Some(landed));
            return (None, false);
        };
        crate::widget::drag_bus::end(None);
        let Some(at) = self.keys.iter().position(|keys| keys.key == lift.key) else {
            return (None, false);
        };
        let next = match &landing.before {
            Some(before) => {
                let Some(next) = self.keys.iter().position(|keys| &keys.key == before) else {
                    return (None, false);
                };
                next
            }
            None => self.keys.len(),
        };
        // A place it already holds moves nothing.
        if next == at || next == at.saturating_add(1) {
            return (None, false);
        }
        (
            Some(DragDrop {
                run: self.id.clone(),
                moved: lift.key,
                at: landing.at,
                before: landing.before,
            }),
            false,
        )
    }

    fn paint(&self, scene: &mut Scene, transform: Affine, _text: &mut TextRenderer, theme: &Theme) {
        let colors = &theme.tokens.colors.intent;
        let radius = f64::from(theme.tokens.radii.control);
        let line_px = f64::from(theme.tokens.metrics.drag_line_px).max(1.0);
        // The line marks where what is dragged would land: the run's own
        // item while it is lifted, and a resource another widget drags over
        // it.
        let landing = if self.is_lifting() {
            self.landing.as_ref()
        } else if self.foreign.is_some() {
            self.landing.as_ref()
        } else {
            None
        };
        if let Some(landing) = landing
            && let Some(line) = self.line_rect(landing, theme)
        {
            scene.fill(
                Fill::NonZero,
                transform,
                colors.accent,
                None,
                &RoundedRect::from_rect(line, line_px / 2.0),
            );
        }
        // The ghost, following the pointer where the press took it: the
        // item's own size while the run's item is lifted, one target while a
        // resource from elsewhere is over it.
        let (size, grab) = if let (true, Some(lift)) = (self.is_lifting(), self.lift.as_ref()) {
            (lift.rect.size(), lift.grab)
        } else if self.foreign.is_some() && self.landing.is_some() {
            let target = f64::from(theme.tokens.metrics.min_pointer_target);
            (
                vello::kurbo::Size::new(target, target),
                Point::new(target / 2.0, target / 2.0),
            )
        } else {
            return;
        };
        let origin = Point::new(self.cursor.x - grab.x, self.cursor.y - grab.y);
        let shape = RoundedRect::from_rect(Rect::from_origin_size(origin, size), radius);
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.overlay,
            None,
            &shape,
        );
        scene.stroke(
            &Stroke::new(line_px),
            transform,
            colors.accent,
            None,
            &shape,
        );
    }
}

/// The payload kind a run's item travels as.
pub const ITEM_PAYLOAD: &str = "drag-run-item";

impl Widget for DragRun {
    fn update(
        &mut self,
        _state: &crate::widget::AppModel,
        _path: &crate::widget::ModelPath,
    ) -> bool {
        self.report();
        // A drag from another run that was let go inside this one is a drop
        // here: one action on the owner's model, as a move of its own is.
        if let Some(landed) = crate::widget::drag_bus::take(&self.id, self.extent()) {
            if let Some(landing) = self.target_at(landed.pointer, None)
                && let crate::dnd::DragPayload::Custom { kind, blob } = landed.payload
                && kind == ITEM_PAYLOAD
                && let Ok(moved) = String::from_utf8(blob)
            {
                self.drops.push(DragDrop {
                    run: self.id.clone(),
                    moved,
                    at: landing.at,
                    before: landing.before,
                });
                return true;
            }
        }
        false
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        if self.items.is_none() {
            let views = std::mem::take(&mut self.views);
            self.items = Some(self.build_items(views));
        }
        let Some(items) = self.items.as_mut() else {
            return taffy.widget_leaf(Style::default(), node_map, self);
        };
        let node = Widget::build_layout(&mut **items, taffy, theme, node_map);
        // The run takes the width it is given, never more: a dock or a bar in a container
        // narrower than its items wraps them (whatever a row's auto width would say) instead
        // of reaching past the container.
        if let Ok(style) = taffy.style(node) {
            let mut style = style.clone();
            style.min_size.width = taffy::Dimension::length(0.0_f32);
            style.flex_shrink = 1.0;
            let _ = taffy.set_style(node, style);
        }
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        if let Some(items) = &mut self.items {
            Widget::apply_layout(&mut **items, taffy, node, origin);
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
        if let Some(items) = &mut self.items {
            Widget::draw(&mut **items, scene, transform, text, theme, presence);
        }
        self.paint(scene, transform, text, theme);
        self.dirty = false;
    }

    fn needs_redraw(&self) -> bool {
        self.dirty
            || self
                .items
                .as_ref()
                .is_some_and(|items| Widget::needs_redraw(&**items))
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
                    // A drag that started cancels the press its item took, so
                    // letting go does not also activate it.
                    if self.is_lifting() {
                        let away = WindowEvent::CursorLeft {
                            device_id: winit::event::DeviceId::dummy(),
                        };
                        if let Some(items) = &mut self.items {
                            Widget::handle_event(&mut **items, &away, ctx);
                        }
                    }
                    *ctx.handled = true;
                    return;
                }
                if self.foreign.is_some() {
                    self.landing = self.target_at(point, None);
                    *ctx.handled = true;
                    return;
                }
            }
            WindowEvent::CursorLeft { .. } => {
                if self.foreign.is_some() {
                    self.landing = None;
                    self.dirty = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                // The press is the item's first: a button in it takes the press as any button
                // does, so letting go without moving presses it.
                if let Some(items) = &mut self.items {
                    Widget::handle_event(&mut **items, event, ctx);
                }
                if self.press(self.cursor) {
                    *ctx.focus_target = Some(ctx.node_id);
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } if self.lift.is_some() => {
                let (drop, click) = self.release();
                if let Some(drop) = drop {
                    self.drops.push(drop);
                }
                // A press that travelled nowhere is the item's own press: its view takes the
                // release too, so a button on a run's item works like any button.
                if !click {
                    *ctx.handled = true;
                    return;
                }
            }
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed
                    && event.logical_key == Key::Named(NamedKey::Escape)
                    && self.lift.is_some() =>
            {
                self.cancel();
                *ctx.handled = true;
                return;
            }
            _ => {}
        }
        if let Some(items) = &mut self.items {
            Widget::handle_event(&mut **items, event, ctx);
        }
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        self.item_at(point).map(|_| winit::window::CursorIcon::Grab)
    }

    fn get_rect(&self) -> Rect {
        self.items
            .as_ref()
            .map_or(Rect::ZERO, |items| Widget::get_rect(&**items))
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, Role};
        self.name
            .as_ref()
            .map(|name| AccessNode::new(Role::Group).with_optional_name(Some(name.clone())))
    }

    fn name(&self) -> &'static str {
        "DragRun"
    }

    fn test_id(&self) -> Option<&str> {
        Some(&self.id)
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.items.iter().map(|items| &**items).collect()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.items
            .iter_mut()
            .map(|items| items.as_mut() as &mut (dyn Widget + '_))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widget::{Container, Spacer};

    fn item(key: &str, width: f32) -> RunItem {
        RunItem {
            key: key.to_owned(),
            label: key.to_owned(),
            item: None,
            view: Box::new(
                Container::new()
                    .with_size(width, 48.0)
                    .with_child(Box::new(Spacer::fixed(1.0))),
            ),
        }
    }

    fn harness(run: DragRun) -> crate::headless::FrameHarness {
        crate::headless::FrameHarness::for_root(crate::State::new(), run, (600, 200), Theme::dark())
    }

    fn run() -> DragRun {
        DragRun::new("dock")
            .name("Dock")
            .push(item("a", 48.0))
            .push(item("b", 48.0))
            .push(item("c", 48.0))
            .push(item("d", 48.0))
    }

    #[test]
    fn dragging_an_item_past_the_second_reports_one_move_before_it() {
        let mut host = harness(run());
        let rects = host.root::<DragRun>().item_rects();
        let (a, c) = (rects[0], rects[2]);
        host.drag_at((a.center().x, a.center().y), (c.x0 + 2.0, c.center().y));
        let drops = host.root_mut::<DragRun>().take_drops();
        assert_eq!(
            drops,
            [DragDrop {
                run: "dock".to_owned(),
                moved: "a".to_owned(),
                at: 1,
                before: Some("c".to_owned()),
            }]
        );
        assert!(!host.root::<DragRun>().is_lifting());
    }

    #[test]
    fn a_click_moves_nothing() {
        let mut host = harness(run());
        let a = host.root::<DragRun>().item_rects()[0];
        host.click_at(a.center().x, a.center().y);
        assert!(host.root_mut::<DragRun>().take_drops().is_empty());
    }

    #[test]
    fn letting_go_where_it_already_stands_moves_nothing() {
        let mut host = harness(run());
        let a = host.root::<DragRun>().item_rects()[0];
        host.drag_at((a.center().x, a.center().y), (a.x1 - 2.0, a.center().y));
        assert!(host.root_mut::<DragRun>().take_drops().is_empty());
    }

    #[test]
    fn dragging_past_the_end_lands_after_the_last_item() {
        let mut host = harness(run());
        let rects = host.root::<DragRun>().item_rects();
        let (a, d) = (rects[0], rects[3]);
        host.drag_at((a.center().x, a.center().y), (d.x1 - 2.0, d.center().y));
        let drops = host.root_mut::<DragRun>().take_drops();
        assert_eq!(drops.len(), 1);
        assert_eq!(drops[0].before, None, "the end of the run");
    }
}
