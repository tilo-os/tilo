//! The canvas as the person sees it: the artwork in a zoomable viewport,
//! drawn on with the pointer. Every finished gesture is one of the
//! artwork's actions (a drag is one, however long), made by [`crate::gesture`];
//! the surface itself never writes the document.
//!
//! It also answers the artwork's views (`look`, `inspect`, `media`,
//! `export`) for callers that have no screen.

#![allow(
    clippy::arithmetic_side_effects,
    reason = "kurbo's geometry operators (Affine, Point, Vec2) are float geometry, not overflowing integer arithmetic"
)]

use std::collections::HashMap;
use std::rc::Rc;

use serde_json::{Value, json};
use ui::a11y::{AccessNode, Role};
use ui::automerge::{AutoCommit, ObjId};
use ui::framework::{Dispatcher, ResourceSurface, SurfaceCx};
use ui::taffy::{self, NodeId, Style, TaffyTree};
use ui::tilo_actions::{self, ActionOutput, InvocationUpdate};
use ui::vello::Scene;
use ui::vello::kurbo::{Affine, BezPath, Circle, Line, Point, Rect, Stroke, Vec2};
use ui::vello::peniko::{Color, Fill};
use ui::widget::{
    AppModel, EventCtx, ModelPath, ResourceOperation, ResourceOperationStart, SelectionHandle,
    SelectionHandles, Widget,
};
use ui::winit::event::{ElementState, MouseButton, WindowEvent};
use ui::winit::keyboard::{Key, ModifiersState, NamedKey};
use ui::winit::window::CursorIcon;
use ui::{LocalState, PresenceCtx, TextRenderer, Theme};

use crate::actions::{CanvasAction, Draw, EditMarks, NewMark, NewStroke, Paint, RemoveMarks};
use crate::gesture::brush::BrushDraft;
use crate::gesture::outline::{Outline, Part, PenDraft, PointRef, constrain_45};
use crate::gesture::snap::{Guide, Snap, snap_point, snap_rect};
use crate::gesture::text::{TextSession, Typed};
use crate::gesture::{Ink, edit};
use crate::model::{Artwork, Mark, Shape};
use crate::render::{Options, Renderer};
use crate::resource::{ArtworkResource, VIEW_TYPE, ViewOp};
use crate::view_ops;

/// The largest SVG `export-svg` also answers inline, in bytes.
const MAX_INLINE_SVG: usize = 48 * 1024;
/// The longest gap between the clicks of a double click.
const DOUBLE_CLICK: ui::time::Duration = ui::time::Duration::from_millis(450);
/// How big a shape made by a click without a drag is, in screen pixels.
const CLICKED_SHAPE: f64 = 100.0; // the size of a mark in the artwork, not on-screen spacing
/// The text size a new text starts at, in screen pixels.
const TEXT_SIZE: f64 = 32.0; // a size of lettering in the artwork, not on-screen spacing
/// How much one wheel notch of a pinch or a zoom key zooms.
const ZOOM_STEP: f64 = 1.25;

/// What the pointer does on the canvas.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tool {
    #[default]
    Select,
    Brush,
    Rect,
    Ellipse,
    Line,
    Pen,
    Text,
    Eyedropper,
}

impl Tool {
    pub const ALL: [Self; 8] = [
        Self::Select,
        Self::Brush,
        Self::Rect,
        Self::Ellipse,
        Self::Line,
        Self::Pen,
        Self::Text,
        Self::Eyedropper,
    ];

    /// The Fluent id of the tool's name.
    pub fn label(self) -> &'static str {
        match self {
            Self::Select => "canvas-tool-select",
            Self::Brush => "canvas-tool-brush",
            Self::Rect => "canvas-tool-rect",
            Self::Ellipse => "canvas-tool-ellipse",
            Self::Line => "canvas-tool-line",
            Self::Pen => "canvas-tool-pen",
            Self::Text => "canvas-tool-text",
            Self::Eyedropper => "canvas-tool-eyedropper",
        }
    }

    /// The key that picks the tool while the canvas has focus.
    pub fn shortcut(self) -> &'static str {
        match self {
            Self::Select => "v",
            Self::Brush => "b",
            Self::Rect => "r",
            Self::Ellipse => "e",
            Self::Line => "l",
            Self::Pen => "p",
            Self::Text => "t",
            Self::Eyedropper => "i",
        }
    }
}

/// The tool settings the rail and inspector share with the canvas.
#[derive(Clone)]
pub struct Tools {
    pub tool: LocalState<Tool>,
    pub colour: LocalState<Color>,
    /// Brush size, `0..=1` of the slider's travel.
    pub size: LocalState<f64>,
    /// The layer new marks go on; the top layer when empty.
    pub layer: LocalState<String>,
}

impl Default for Tools {
    fn default() -> Self {
        Self {
            tool: LocalState::new(Tool::Brush),
            // The brush starts loaded with ultramarine: a paint colour, not
            // chrome, so it does not follow the theme.
            colour: LocalState::new(Color::from_rgba8(0x2b, 0x3f, 0x8f, 255)), // paint
            size: LocalState::new(0.2),
            layer: LocalState::new(String::new()),
        }
    }
}

impl Tools {
    /// The brush width in artwork units.
    pub fn brush_size(&self) -> f64 {
        let travel = self.size.with(|size| size.clamp(0.0, 1.0));
        1.0 + travel * travel * 119.0
    }

    pub fn hex(&self) -> String {
        self.colour.with(|colour| hex(*colour))
    }

    /// What the tools put down now.
    pub fn ink(&self) -> Ink {
        Ink {
            colour: self.hex(),
            width: self.brush_size(),
            layer: Some(self.layer.with(Clone::clone)).filter(|layer| !layer.is_empty()),
        }
    }
}

fn hex(colour: Color) -> String {
    let rgba = colour.to_rgba8();
    format!("#{:02x}{:02x}{:02x}", rgba.r, rgba.g, rgba.b)
}

/// A pointer drag in progress.
enum Drag {
    Pan,
    /// A box drawn over empty canvas picks what it touches; `base` is what
    /// was picked before when adding to it.
    Marquee {
        start: Point,
        now: Point,
        base: Vec<String>,
    },
    /// The selection carried along; `affine` is where it is now, snapped.
    Move {
        start: Point,
        now: Point,
        ids: Vec<String>,
        frame: Rect,
        targets: Vec<Rect>,
        affine: Affine,
    },
    /// A grip of the selection frame: a stretch or a turn.
    Handle {
        handle: SelectionHandle,
        frame: SelectionHandles,
        start: Point,
        now: Point,
        ids: Vec<String>,
        targets: Vec<Rect>,
        affine: Affine,
    },
    Stroke {
        draft: BrushDraft,
        id: String,
        ink: Ink,
    },
    Shape {
        start: Point,
        now: Point,
    },
    /// The pen is down, pulling the newest anchor's handles.
    Pen,
    /// A point of the path being edited, in the mark's own coordinates.
    Point {
        point: PointRef,
        start: Point,
        moved: bool,
    },
}

/// A path's points being edited: its outline in the mark's own coordinates.
struct PointEdit {
    id: String,
    shape: Shape,
    affine: Affine,
    outline: Outline,
    active: Option<PointRef>,
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "permission, focus, a held space key and a hand-moved view are independent states"
)]
pub struct CanvasSurface {
    rect: Rect,
    art: Rc<Artwork>,
    dispatcher: Option<Dispatcher<ArtworkResource>>,
    read_only: bool,
    viewport: ui::widget::ViewportState,
    /// The viewport and artwork sizes the view was last fitted to; the
    /// view follows them until the person zooms or pans.
    fitted_to: Option<(f64, f64, f64, f64)>,
    moved_by_hand: bool,
    renderer: Renderer,
    /// Shapes text for looks and exports, which run outside a frame.
    look_text: Option<TextRenderer>,
    tools: Tools,
    /// The tool the modes below belong to; a change of tool ends them.
    tool: Tool,
    drag: Option<Drag>,
    selection: Vec<String>,
    /// Marks just made or pasted, selected before the artwork has them.
    expected: Vec<String>,
    pen: Option<PenDraft>,
    points: Option<PointEdit>,
    text: Option<TextSession>,
    guides: Vec<Guide>,
    cursor: Point,
    /// Where and when the last press was, to tell a double click.
    last_press: Option<(Point, ui::time::Instant)>,
    /// When the canvas started, the clock brush speed is judged on.
    epoch: ui::time::Instant,
    /// Pastes since the last copy: each lands a step further along.
    pastes: u32,
    /// An area a screenshot scenario asks to be shown, once laid out.
    staged_view: Option<Rect>,
    focused: bool,
    space: bool,
    modifiers: ModifiersState,
    results: HashMap<tilo_actions::InvocationId, InvocationUpdate>,
    /// The theme's sizes as of the last layout, for handles hit-tested
    /// between frames.
    metrics: ui::tokens::Metrics,
    /// The theme's spacing: how far a click may wander, how near an edge
    /// snaps, how far a copy lands from its original.
    spacing: ui::tokens::Spacing,
}

impl CanvasSurface {
    pub fn new(tools: Tools) -> Self {
        let tool = tools.tool.with(|tool| *tool);
        Self {
            rect: Rect::ZERO,
            art: Rc::new(Artwork::default()),
            dispatcher: None,
            read_only: false,
            viewport: ui::widget::ViewportState::new()
                .with_zoom_bounds(0.05, 64.0)
                .with_padding(24.0),
            fitted_to: None,
            moved_by_hand: false,
            renderer: Renderer::default(),
            look_text: None,
            tools,
            tool,
            drag: None,
            selection: Vec::new(),
            expected: Vec::new(),
            pen: None,
            points: None,
            text: None,
            guides: Vec::new(),
            cursor: Point::ZERO,
            last_press: None,
            epoch: ui::time::now(),
            pastes: 0,
            staged_view: None,
            focused: false,
            space: false,
            modifiers: ModifiersState::empty(),
            results: HashMap::new(),
            metrics: ui::tokens::Metrics::BASE,
            spacing: ui::tokens::Spacing::BASE,
        }
    }

    fn dispatch(&self, action: &CanvasAction) {
        if self.read_only {
            return;
        }
        if let Some(dispatcher) = &self.dispatcher {
            dispatcher.apply(action);
        }
    }

    /// Several actions as one step of history.
    fn dispatch_all(&self, actions: &[CanvasAction]) {
        if self.read_only {
            return;
        }
        if let Some(dispatcher) = &self.dispatcher {
            dispatcher.group(|| {
                for action in actions {
                    dispatcher.apply(action);
                }
            });
        }
    }

    fn content_point(&self, screen: Point) -> Point {
        self.viewport.screen_to_content(screen)
    }

    fn zoom(&self) -> f64 {
        self.viewport.zoom().max(1e-6)
    }

    /// `pixels` on screen in artwork units.
    fn units(&self, pixels: f64) -> f64 {
        pixels / self.zoom()
    }

    /// How far the pointer may travel, in screen pixels, and still click.
    fn click_slop(&self) -> f64 {
        f64::from(self.spacing.s1)
    }

    /// How near, in screen pixels, a dragged edge snaps to another.
    fn snap_reach(&self) -> f64 {
        f64::from(self.spacing.s2)
    }

    /// How far a duplicate or paste lands from its original, in screen
    /// pixels.
    fn copy_offset(&self) -> f64 {
        f64::from(self.spacing.s4)
    }

    fn current_tool(&self) -> Tool {
        self.tools.tool.with(|tool| *tool)
    }

    fn select(&mut self, ids: Vec<String>) {
        self.selection = ids;
        self.expected.clear();
    }

    /// Select marks the artwork will have once `actions` land.
    fn select_made(&mut self, ids: Vec<String>) {
        self.expected.clone_from(&ids);
        self.selection = ids;
    }

    /// The frame around the selected marks.
    fn frame(&self) -> Option<SelectionHandles> {
        if self.points.is_some() {
            return None;
        }
        let ids: Vec<String> = self
            .selection
            .iter()
            .filter(|id| self.text.as_ref().is_none_or(|text| text.id != **id))
            .cloned()
            .collect();
        edit::frame(&self.art, &ids)
            .map(|(bounds, angle)| SelectionHandles::new(bounds, &self.metrics).turned(angle))
    }

    fn ink(&self) -> Ink {
        let mut ink = self.tools.ink();
        if ink
            .layer
            .as_deref()
            .is_some_and(|layer| self.art.layer(layer).is_none())
        {
            ink.layer = None;
        }
        ink
    }

    /// Whether marks on `layer` (the top layer when absent) are painted by
    /// a medium rather than drawn.
    fn painted(&self, layer: Option<&str>) -> bool {
        let layer = match layer {
            Some(id) => self.art.layer(id),
            None => self.art.layers.last(),
        };
        layer.is_some_and(|layer| {
            !layer.medium.is_empty() && crate::media::resolve(&self.art, &layer.medium).is_some()
        })
    }

    fn mark_painted(&self, mark: &Mark) -> bool {
        self.painted(Some(&mark.layer))
    }

    /// End whatever the last tool left open when the tool changes.
    fn follow_tool(&mut self) {
        let tool = self.current_tool();
        if tool == self.tool {
            return;
        }
        self.tool = tool;
        self.finish_pen();
        self.finish_text();
        if tool != Tool::Select {
            self.points = None;
        }
    }

    fn finish_pen(&mut self) {
        let Some(pen) = self.pen.take() else {
            return;
        };
        let Some(shape) = pen.shape() else {
            return;
        };
        let ink = self.ink();
        let id = edit::fresh_id(&self.art, "path", &[]);
        let closed = pen.contour.closed;
        self.dispatch(&CanvasAction::Draw(Draw {
            layer: ink.layer,
            marks: vec![NewMark {
                id: Some(id.clone()),
                fill: closed.then(|| ink.colour.clone()),
                stroke: (!closed).then_some(ink.colour),
                stroke_width: (!closed).then_some(ink.width),
                ..NewMark::new(shape)
            }],
        }));
        self.select_made(vec![id]);
    }

    fn finish_text(&mut self) {
        let Some(session) = self.text.take() else {
            return;
        };
        if let Some(action) = session.commit(&self.art) {
            self.dispatch(&action);
            if session.new {
                self.select_made(vec![session.id]);
            }
        }
    }

    fn start_points(&mut self, id: &str) -> bool {
        let Some(mark) = self.art.mark(id) else {
            return false;
        };
        let Some(outline) = Outline::of_shape(&mark.shape) else {
            return false;
        };
        self.points = Some(PointEdit {
            id: id.to_owned(),
            shape: mark.shape.clone(),
            affine: mark.affine(),
            outline,
            active: None,
        });
        self.select(vec![id.to_owned()]);
        true
    }

    fn start_text_edit(&mut self, id: &str) -> bool {
        match TextSession::edit(&self.art, id) {
            Some(session) => {
                self.text = Some(session);
                self.select(vec![id.to_owned()]);
                true
            }
            None => false,
        }
    }

    /// Enter a mark: edit a path's points or a text's words.
    fn enter(&mut self, id: &str) -> bool {
        if self.read_only {
            return false;
        }
        self.start_text_edit(id) || self.start_points(id)
    }

    fn commit_points(&mut self) {
        let Some(points) = &self.points else {
            return;
        };
        if points.outline.is_empty() {
            let id = points.id.clone();
            self.points = None;
            self.dispatch(&CanvasAction::RemoveMarks(RemoveMarks {
                ids: vec![id],
                layer: None,
            }));
            return;
        }
        let shape = points.outline.to_shape(&points.shape);
        if shape != points.shape {
            let action = CanvasAction::edit(EditMarks {
                ids: vec![points.id.clone()],
                shape: Some(shape.clone()),
                ..EditMarks::default()
            });
            if let Some(points) = &mut self.points {
                points.shape = shape;
            }
            self.dispatch(&action);
        }
    }

    fn double_click(&mut self, screen: Point) -> bool {
        let now = ui::time::now();
        let double = self.last_press.is_some_and(|(at, then)| {
            at.distance(screen) <= self.click_slop() * 2.0
                && now.duration_since(then) <= DOUBLE_CLICK
        });
        self.last_press = if double { None } else { Some((screen, now)) };
        double
    }

    fn press(&mut self, screen: Point) {
        self.follow_tool();
        let double = self.double_click(screen);
        let at = self.content_point(screen);
        let tool = self.current_tool();
        if self.space || self.read_only && tool != Tool::Select {
            self.viewport.begin_pan(screen);
            self.drag = Some(Drag::Pan);
            return;
        }
        if self.text.is_some() {
            let inside = self.text.as_ref().is_some_and(|text| {
                text.shape
                    .text_box()
                    .is_some_and(|area| area.inflate(self.units(8.0), self.units(8.0)).contains(at))
            });
            if inside {
                return;
            }
            self.finish_text();
            if tool == Tool::Text {
                return;
            }
        }
        if self.points.is_some() && self.press_points(at, double) {
            return;
        }
        self.drag = match tool {
            Tool::Select => self.press_select(at, double),
            Tool::Brush => Some(Drag::Stroke {
                draft: BrushDraft::new(at),
                id: edit::fresh_id(&self.art, "s", &[]),
                ink: self.ink(),
            }),
            Tool::Rect | Tool::Ellipse | Tool::Line => {
                let start = self.snapped(at, &[]).0;
                Some(Drag::Shape { start, now: start })
            }
            Tool::Pen => {
                let reach = f64::from(self.metrics.min_pointer_target) / 2.0 / self.zoom();
                let pen = self.pen.get_or_insert_with(PenDraft::default);
                let at = match (self.modifiers.shift_key(), pen.contour.anchors.last()) {
                    (true, Some(last)) => constrain_45(last.at, at),
                    _ => at,
                };
                pen.press(at, reach);
                Some(Drag::Pen)
            }
            Tool::Text => {
                if let Some(mark) = self.art.mark_at(at)
                    && matches!(mark.shape, Shape::Text { .. })
                {
                    let id = mark.id.clone();
                    self.start_text_edit(&id);
                } else {
                    let ink = self.ink();
                    let size = (self.units(TEXT_SIZE)).round().max(1.0);
                    self.text = Some(TextSession::new(
                        edit::fresh_id(&self.art, "text", &[]),
                        (at.x, at.y + size * 0.35),
                        size,
                        ink.colour,
                        ink.layer,
                    ));
                    self.select(Vec::new());
                }
                None
            }
            Tool::Eyedropper => {
                let colour = self.renderer.sample(&self.art, at);
                self.tools.colour.set(colour);
                None
            }
        };
    }

    /// A press while a path's points are edited: `true` when it was the
    /// editor's.
    fn press_points(&mut self, at: Point, double: bool) -> bool {
        let reach_screen = f64::from(self.metrics.min_pointer_target) / 2.0;
        let zoom = self.zoom();
        let alt = self.modifiers.alt_key();
        let Some(points) = &mut self.points else {
            return false;
        };
        let local = points.affine.inverse() * at;
        let stretch = points.affine.determinant().abs().sqrt().max(1e-6);
        let reach = reach_screen / zoom / stretch;
        if let Some(point) = points.outline.hit(local, reach, points.active) {
            if alt && point.part == Part::Anchor {
                points.outline.toggle_smooth(point.contour, point.anchor);
                points.active = Some(point);
                self.commit_points();
                return true;
            }
            points.active = Some(PointRef {
                part: Part::Anchor,
                ..point
            });
            self.drag = Some(Drag::Point {
                point,
                start: local,
                moved: false,
            });
            return true;
        }
        if double && let Some(point) = points.outline.insert_near(local, reach) {
            points.active = Some(point);
            self.commit_points();
            return true;
        }
        let on_mark = self.art.mark(&points.id).is_some_and(|mark| mark.hit(at));
        if on_mark {
            points.active = None;
            return true;
        }
        self.points = None;
        false
    }

    fn press_select(&mut self, at: Point, double: bool) -> Option<Drag> {
        let zoom = self.zoom();
        if let Some(frame) = self.frame()
            && let Some(handle) = frame.handle_at(at, zoom)
        {
            return Some(Drag::Handle {
                handle,
                frame,
                start: at,
                now: at,
                ids: self.selection.clone(),
                targets: edit::snap_targets(&self.art, &self.selection),
                affine: Affine::IDENTITY,
            });
        }
        let inside_group = double || self.modifiers.control_key();
        let hit = edit::pick(&self.art, at, inside_group);
        if double && let [id] = hit.as_slice() {
            let id = id.clone();
            if self.enter(&id) {
                return None;
            }
            self.select(vec![id]);
        } else if self.modifiers.shift_key() {
            if hit.is_empty() {
                return Some(Drag::Marquee {
                    start: at,
                    now: at,
                    base: self.selection.clone(),
                });
            }
            if hit.iter().all(|id| self.selection.contains(id)) {
                self.selection.retain(|id| !hit.contains(id));
            } else {
                let mut selection = self.selection.clone();
                selection.extend(hit.into_iter().filter(|id| !self.selection.contains(id)));
                self.select(selection);
            }
            return None;
        } else if hit.is_empty() {
            let in_frame =
                self.selection.len() > 1 && self.frame().is_some_and(|frame| frame.contains(at));
            if !in_frame {
                self.select(Vec::new());
                return Some(Drag::Marquee {
                    start: at,
                    now: at,
                    base: Vec::new(),
                });
            }
        } else if !hit.iter().all(|id| self.selection.contains(id)) {
            self.select(hit);
        }
        if self.selection.is_empty() {
            return None;
        }
        let frame =
            edit::frame(&self.art, &self.selection).map_or(Rect::ZERO, |(bounds, _)| bounds);
        Some(Drag::Move {
            start: at,
            now: at,
            ids: self.selection.clone(),
            frame,
            targets: edit::snap_targets(&self.art, &self.selection),
            affine: Affine::IDENTITY,
        })
    }

    /// `at` snapped to `targets` (the artwork and every mark other than
    /// `moving` when empty), unless Ctrl holds snapping off.
    fn snapped(&self, at: Point, targets: &[Rect]) -> (Point, Vec<Guide>) {
        if self.modifiers.control_key() {
            return (at, Vec::new());
        }
        let found;
        let targets = if targets.is_empty() {
            found = edit::snap_targets(&self.art, &[]);
            &found
        } else {
            targets
        };
        let Snap { offset, guides } = snap_point(at, targets, self.units(self.snap_reach()));
        (at + offset, guides)
    }

    fn drag_to(&mut self, screen: Point) {
        let slop = self.click_slop();
        let at = self.content_point(screen);
        let zoom = self.zoom();
        let seconds = ui::time::now().duration_since(self.epoch).as_secs_f64();
        match &mut self.drag {
            Some(Drag::Pan) => {
                self.moved_by_hand |= self.viewport.update_pan(screen);
            }
            Some(
                Drag::Move { now, .. }
                | Drag::Shape { now, .. }
                | Drag::Handle { now, .. }
                | Drag::Marquee { now, .. },
            ) => {
                *now = at;
            }
            Some(Drag::Stroke { draft, .. }) => draft.push(at, seconds, zoom),
            Some(Drag::Pen) => {
                if let Some(pen) = &mut self.pen {
                    pen.pull(at);
                }
            }
            Some(Drag::Point {
                point,
                start,
                moved,
            }) => {
                if let Some(points) = &mut self.points {
                    let local = points.affine.inverse() * at;
                    let stretch = points.affine.determinant().abs().sqrt().max(1e-6);
                    if *moved || local.distance(*start) * zoom * stretch > slop {
                        *moved = true;
                        points.outline.set(*point, local);
                    }
                }
            }
            None => {}
        }
        self.update_drag();
    }

    /// Work out where the dragged selection is, with snapping and the held
    /// keys, after the pointer or the keys changed.
    fn update_drag(&mut self) {
        let shift = self.modifiers.shift_key();
        let alt = self.modifiers.alt_key();
        let free = self.modifiers.control_key();
        let reach = self.units(self.snap_reach());
        let mut guides = Vec::new();
        match &mut self.drag {
            Some(Drag::Move {
                start,
                now,
                frame,
                targets,
                affine,
                ..
            }) => {
                let mut delta = *now - *start;
                if shift {
                    if delta.x.abs() > delta.y.abs() {
                        delta.y = 0.0;
                    } else {
                        delta.x = 0.0;
                    }
                }
                if !free {
                    let snap = snap_rect(*frame + delta, targets, reach);
                    let offset = if shift {
                        if delta.y == 0.0 {
                            Vec2::new(snap.offset.x, 0.0)
                        } else {
                            Vec2::new(0.0, snap.offset.y)
                        }
                    } else {
                        snap.offset
                    };
                    delta += offset;
                    guides = snap_rect(*frame + delta, targets, 1e-6).guides;
                }
                *affine = Affine::translate(delta);
            }
            Some(Drag::Handle {
                handle,
                frame,
                start,
                now,
                targets,
                affine,
                ..
            }) => {
                if *handle == SelectionHandle::Rotate {
                    *affine = frame.rotation_affine(*start, *now, shift);
                } else {
                    let mut to = *now;
                    if !free && frame.angle == 0.0 && !shift {
                        let snap = snap_point(to, targets, reach);
                        to += snap.offset;
                        guides = snap.guides;
                    }
                    *affine = frame.stretch_for(*handle, (*start, to), shift, alt);
                }
            }
            Some(Drag::Shape { now, .. }) if !free => {
                let snap = snap_point(*now, &edit::snap_targets(&self.art, &[]), reach);
                guides = snap.guides;
            }
            _ => {}
        }
        self.guides = guides;
    }

    /// The box a shape drag makes: square with Shift, about its start with
    /// Alt.
    fn shape_rect(&self, start: Point, now: Point) -> Rect {
        let now = if self.modifiers.control_key() {
            now
        } else {
            self.snapped(now, &[]).0
        };
        let mut delta = now - start;
        if self.modifiers.shift_key() {
            let side = delta.x.abs().max(delta.y.abs());
            delta = Vec2::new(side.copysign(delta.x), side.copysign(delta.y));
        }
        if self.modifiers.alt_key() {
            Rect::from_points(start - delta, start + delta)
        } else {
            Rect::from_points(start, start + delta)
        }
    }

    /// A line drag's end: on an eighth of a turn with Shift.
    fn line_end(&self, start: Point, now: Point) -> Point {
        if self.modifiers.shift_key() {
            constrain_45(start, now)
        } else if self.modifiers.control_key() {
            now
        } else {
            self.snapped(now, &[]).0
        }
    }

    fn release(&mut self) {
        let Some(drag) = self.drag.take() else {
            return;
        };
        self.guides.clear();
        let zoom = self.zoom();
        match drag {
            Drag::Pan => {
                self.viewport.end_pan();
            }
            Drag::Marquee { start, now, base } => {
                let area = Rect::from_points(start, now);
                if area.width().max(area.height()) * zoom < self.click_slop() {
                    return;
                }
                let mut picked = base;
                for id in edit::marquee(&self.art, area) {
                    if !picked.contains(&id) {
                        picked.push(id);
                    }
                }
                self.select(picked);
            }
            Drag::Move {
                start,
                now,
                ids,
                affine,
                ..
            } => {
                if start.distance(now) * zoom < self.click_slop() {
                    return;
                }
                if self.modifiers.alt_key() {
                    let (actions, made) = edit::duplicate(&self.art, &ids, affine.translation());
                    self.dispatch_all(&actions);
                    self.select_made(made);
                } else if let Some(action) = edit::transform(ids, affine) {
                    self.dispatch(&action);
                }
            }
            Drag::Handle {
                start,
                now,
                ids,
                affine,
                ..
            } => {
                if start.distance(now) * zoom >= 1.0
                    && let Some(action) = edit::transform(ids, affine)
                {
                    self.dispatch(&action);
                }
            }
            Drag::Stroke { draft, id, ink } => {
                self.dispatch(&CanvasAction::Paint(Paint {
                    layer: ink.layer,
                    strokes: vec![NewStroke {
                        id: Some(id),
                        name: None,
                        points: draft.finish(),
                        size: ink.width,
                        colour: ink.colour,
                        opacity: None,
                        wet: Some(true),
                        params: None,
                    }],
                }));
            }
            Drag::Shape { start, now } => self.finish_shape(start, now),
            Drag::Pen => {
                if let Some(pen) = &mut self.pen {
                    pen.release();
                    if pen.contour.closed {
                        self.finish_pen();
                        self.tools.tool.set(Tool::Select);
                        self.tool = Tool::Select;
                    }
                }
            }
            Drag::Point { moved, .. } => {
                if moved {
                    self.commit_points();
                }
            }
        }
    }

    fn finish_shape(&mut self, start: Point, now: Point) {
        let tool = self.current_tool();
        let ink = self.ink();
        let clicked = start.distance(now) * self.zoom() < self.click_slop();
        let shape = if tool == Tool::Line {
            let end = if clicked {
                start + Vec2::new(self.units(CLICKED_SHAPE), 0.0)
            } else {
                self.line_end(start, now)
            };
            Shape::Line {
                points: vec![[start.x, start.y], [end.x, end.y]],
            }
        } else {
            let rect = if clicked {
                Rect::from_origin_size(
                    start,
                    (self.units(CLICKED_SHAPE), self.units(CLICKED_SHAPE)),
                )
            } else {
                self.shape_rect(start, now)
            };
            if rect.width() < 1e-3 || rect.height() < 1e-3 {
                return;
            }
            if tool == Tool::Ellipse {
                Shape::Ellipse {
                    cx: rect.center().x,
                    cy: rect.center().y,
                    rx: rect.width() / 2.0,
                    ry: rect.height() / 2.0,
                }
            } else {
                Shape::Rect {
                    x: rect.x0,
                    y: rect.y0,
                    width: rect.width(),
                    height: rect.height(),
                    radius: 0.0,
                }
            }
        };
        let id = edit::fresh_id(&self.art, shape.kind(), &[]);
        let line = tool == Tool::Line;
        self.dispatch(&CanvasAction::Draw(Draw {
            layer: ink.layer,
            marks: vec![NewMark {
                id: Some(id.clone()),
                fill: (!line).then(|| ink.colour.clone()),
                stroke: line.then_some(ink.colour),
                stroke_width: line.then_some(ink.width),
                ..NewMark::new(shape)
            }],
        }));
        self.select_made(vec![id]);
        self.tools.tool.set(Tool::Select);
        self.tool = Tool::Select;
    }

    fn zoom_by(&mut self, factor: f64, focal: Option<Point>) {
        match focal {
            Some(focal) => self.viewport.zoom_at(factor, focal),
            None => self.viewport.zoom_by(factor),
        }
        self.moved_by_hand = true;
    }

    fn zoom_to_fit(&mut self) {
        self.viewport.reset_to_fit();
        self.moved_by_hand = false;
    }

    fn zoom_to_selection(&mut self) {
        if let Some((bounds, _)) = edit::frame(&self.art, &self.selection) {
            let margin = bounds.width().max(bounds.height()) * 0.1;
            self.viewport.zoom_to_rect(bounds.inflate(margin, margin));
            self.moved_by_hand = true;
        }
    }

    /// The view keys: zoom in and out, to fit, to 100 %, to the selection.
    fn view_key(&mut self, key: &Key, modifiers: ModifiersState) -> bool {
        let Key::Character(ch) = key else {
            return false;
        };
        let command = modifiers.control_key() || modifiers.super_key();
        match (ch.as_str(), command, modifiers.shift_key()) {
            ("+" | "=", _, _) => self.zoom_by(ZOOM_STEP, None),
            ("-" | "_", _, _) => self.zoom_by(1.0 / ZOOM_STEP, None),
            ("0", true, _) | ("!" | "1", false, true) => self.zoom_to_fit(),
            ("1", true, _) | ("0" | ")", false, true) => {
                self.viewport.set_zoom(1.0);
                self.moved_by_hand = true;
            }
            ("2" | "@", false, true) => self.zoom_to_selection(),
            _ => return false,
        }
        true
    }

    fn key(&mut self, key: &Key, typed: Option<&str>, modifiers: ModifiersState) -> bool {
        self.follow_tool();
        if let Some(session) = &mut self.text {
            match session.key(key, typed, modifiers) {
                Typed::Edited => return true,
                Typed::Done => {
                    self.finish_text();
                    return true;
                }
                Typed::Ignored => {}
            }
        }
        if self.view_key(key, modifiers) {
            return true;
        }
        let command = modifiers.control_key() || modifiers.super_key();
        if command && let Key::Character(ch) = key {
            return self.command_key(&ch.to_lowercase(), modifiers.shift_key());
        }
        if self.points.is_some() && self.points_key(key, modifiers) {
            return true;
        }
        if self.pen.is_some() {
            match key {
                Key::Named(NamedKey::Enter | NamedKey::Escape) => {
                    self.finish_pen();
                    return true;
                }
                Key::Named(NamedKey::Backspace | NamedKey::Delete) => {
                    if let Some(pen) = &mut self.pen
                        && !pen.undo_anchor()
                    {
                        self.pen = None;
                    }
                    return true;
                }
                _ => {}
            }
        }
        let nudge = if modifiers.shift_key() { 10.0 } else { 1.0 };
        let step = match key {
            Key::Named(NamedKey::ArrowLeft) => Some([-nudge, 0.0]),
            Key::Named(NamedKey::ArrowRight) => Some([nudge, 0.0]),
            Key::Named(NamedKey::ArrowUp) => Some([0.0, -nudge]),
            Key::Named(NamedKey::ArrowDown) => Some([0.0, nudge]),
            _ => None,
        };
        match key {
            _ if step.is_some() && !self.selection.is_empty() => {
                self.dispatch(&CanvasAction::edit(EditMarks {
                    ids: self.selection.clone(),
                    move_by: step,
                    ..EditMarks::default()
                }));
                true
            }
            Key::Named(NamedKey::Delete | NamedKey::Backspace) if !self.selection.is_empty() => {
                let ids = std::mem::take(&mut self.selection);
                self.dispatch(&CanvasAction::RemoveMarks(RemoveMarks { ids, layer: None }));
                true
            }
            Key::Named(NamedKey::Enter) if self.selection.len() == 1 => {
                let id = self.selection.first().cloned().unwrap_or_default();
                self.enter(&id)
            }
            Key::Named(NamedKey::Escape) if !self.selection.is_empty() => {
                self.select(Vec::new());
                true
            }
            Key::Character(ch) if !modifiers.alt_key() => {
                let Some(tool) = Tool::ALL
                    .into_iter()
                    .find(|tool| tool.shortcut() == ch.to_lowercase())
                else {
                    return false;
                };
                self.tools.tool.set(tool);
                self.follow_tool();
                true
            }
            _ => {
                if self.viewport.on_key(key, &modifiers) {
                    self.moved_by_hand = true;
                    return true;
                }
                false
            }
        }
    }

    /// Ctrl/Cmd with a letter: the clipboard, select all, duplicate, group.
    fn command_key(&mut self, ch: &str, shift: bool) -> bool {
        let offset = Vec2::new(
            self.units(self.copy_offset()),
            self.units(self.copy_offset()),
        );
        match (ch, shift) {
            ("a", _) => {
                self.select(edit::everything(&self.art));
            }
            ("c" | "x", _) => {
                let Some(text) = edit::copy(&self.art, &self.selection) else {
                    return false;
                };
                ui::clipboard_copy(&text);
                self.pastes = 0;
                if ch == "x" {
                    let ids = std::mem::take(&mut self.selection);
                    self.dispatch(&CanvasAction::RemoveMarks(RemoveMarks { ids, layer: None }));
                }
            }
            ("v", _) => {
                let Some(text) = ui::clipboard_paste() else {
                    return false;
                };
                self.pastes = self.pastes.saturating_add(1);
                let ink = self.ink();
                let Some((actions, made)) = edit::paste(
                    &self.art,
                    &text,
                    offset * f64::from(self.pastes),
                    ink.layer
                        .as_deref()
                        .or_else(|| self.art.layers.last().map(|layer| layer.id.as_str())),
                ) else {
                    return false;
                };
                self.dispatch_all(&actions);
                self.select_made(made);
            }
            ("d", false) => {
                if self.selection.is_empty() {
                    return false;
                }
                let (actions, made) = edit::duplicate(&self.art, &self.selection, offset);
                self.dispatch_all(&actions);
                self.select_made(made);
            }
            ("g", false) => {
                let Some(action) = edit::group(&self.art, &self.selection) else {
                    return false;
                };
                self.dispatch(&action);
            }
            ("g", true) => {
                let Some(action) = edit::ungroup(&self.art, &self.selection) else {
                    return false;
                };
                self.dispatch(&action);
            }
            _ => return false,
        }
        true
    }

    /// Keys while a path's points are edited: nudge or remove the point,
    /// leave with Escape or Enter.
    fn points_key(&mut self, key: &Key, modifiers: ModifiersState) -> bool {
        let nudge = if modifiers.shift_key() { 10.0 } else { 1.0 };
        let Some(points) = &mut self.points else {
            return false;
        };
        let step = match key {
            Key::Named(NamedKey::ArrowLeft) => Vec2::new(-nudge, 0.0),
            Key::Named(NamedKey::ArrowRight) => Vec2::new(nudge, 0.0),
            Key::Named(NamedKey::ArrowUp) => Vec2::new(0.0, -nudge),
            Key::Named(NamedKey::ArrowDown) => Vec2::new(0.0, nudge),
            Key::Named(NamedKey::Escape | NamedKey::Enter) => {
                self.points = None;
                return true;
            }
            Key::Named(NamedKey::Delete | NamedKey::Backspace) => {
                let Some(active) = points.active.take() else {
                    return false;
                };
                points.outline.remove(active);
                self.commit_points();
                return true;
            }
            _ => return false,
        };
        let Some(active) = points.active else {
            return false;
        };
        let local = points.affine.inverse();
        let Some(at) = points.outline.get(active) else {
            return false;
        };
        let moved = local * (points.affine * at + step);
        points.outline.set(active, moved);
        self.commit_points();
        true
    }

    /// The artwork as shown while something is being changed: the moved
    /// marks where they are going, the path as edited, the text as typed.
    /// `None` when it is the artwork as it is.
    fn shown(&self) -> Option<Artwork> {
        let mut shown: Option<Artwork> = None;
        match &self.drag {
            Some(Drag::Move { ids, affine, .. } | Drag::Handle { ids, affine, .. })
                if *affine != Affine::IDENTITY =>
            {
                let art = shown.get_or_insert_with(|| (*self.art).clone());
                for mark in &mut art.marks {
                    if ids.contains(&mark.id) && !self.mark_painted(mark) {
                        *mark = mark.transformed(*affine);
                    }
                }
            }
            _ => {}
        }
        if let Some(points) = &self.points {
            let shape = points.outline.to_shape(&points.shape);
            if self
                .art
                .mark(&points.id)
                .is_some_and(|mark| mark.shape != shape)
            {
                let art = shown.get_or_insert_with(|| (*self.art).clone());
                if let Some(mark) = art.marks.iter_mut().find(|mark| mark.id == points.id) {
                    mark.shape = shape;
                }
            }
        }
        if let Some(text) = &self.text {
            let art = shown.get_or_insert_with(|| (*self.art).clone());
            if let Some(mark) = art.marks.iter_mut().find(|mark| mark.id == text.id) {
                mark.shape = text.shape.clone();
            } else {
                let layer = text
                    .layer
                    .clone()
                    .or_else(|| art.layers.last().map(|layer| layer.id.clone()))
                    .unwrap_or_default();
                art.marks.push(Mark {
                    id: text.id.clone(),
                    layer,
                    order: String::new(),
                    name: String::new(),
                    shape: text.shape.clone(),
                    style: crate::model::Style {
                        fill: Some(text.fill.clone()),
                        ..crate::model::Style::default()
                    },
                    transform: None,
                    group: String::new(),
                    clip: String::new(),
                });
            }
        }
        shown
    }

    /// The stroke being painted on a painted layer, as the medium will
    /// paint it.
    fn wet(&self) -> Option<Mark> {
        let Some(Drag::Stroke { draft, id, ink }) = &self.drag else {
            return None;
        };
        if !self.painted(ink.layer.as_deref()) {
            return None;
        }
        let layer = ink
            .layer
            .clone()
            .or_else(|| self.art.layers.last().map(|layer| layer.id.clone()))?;
        Some(Mark {
            id: id.clone(),
            layer,
            order: String::new(),
            name: String::new(),
            shape: Shape::Stroke {
                points: draft.preview(),
                size: ink.width,
            },
            style: crate::model::Style {
                stroke: Some(ink.colour.clone()),
                wet: true,
                ..crate::model::Style::default()
            },
            transform: None,
            group: String::new(),
            clip: String::new(),
        })
    }

    /// The revision a view answers about: the document's heads.
    fn revision(doc: &mut AutoCommit) -> String {
        let mut hasher = blake3::Hasher::new();
        for head in doc.get_heads() {
            hasher.update(head.as_ref());
        }
        hasher
            .finalize()
            .to_hex()
            .get(..12)
            .unwrap_or_default()
            .to_owned()
    }

    /// The artwork at `version` (a change id or a checkpoint name), its
    /// revision and the version's name; now when `version` is absent.
    fn at_version(
        doc: &mut AutoCommit,
        version: Option<&str>,
    ) -> Result<(Artwork, String, Option<String>), String> {
        let Some(version) = version else {
            return Ok((crate::doc::read(doc), Self::revision(doc), None));
        };
        let heads = ui::history_source::steps::resolve(doc, version)?;
        let mut past = doc
            .fork_at(&heads)
            .map_err(|error| format!("version {version:?} is not in this artwork: {error}"))?;
        let name = ui::framework::history::version_name(&heads);
        Ok((
            crate::doc::read(&past),
            Self::revision(&mut past),
            Some(name),
        ))
    }

    /// The two versions a diff compares; `to` is now when absent.
    fn versions(
        doc: &mut AutoCommit,
        now: &Artwork,
        diff: &crate::resource::Diff,
    ) -> Result<view_ops::Versions, String> {
        let (before, _, from) = Self::at_version(doc, Some(&diff.from))?;
        let (after, to) = match &diff.to {
            Some(to) => {
                let (after, _, to) = Self::at_version(doc, Some(to))?;
                (after, to)
            }
            None => (now.clone(), None),
        };
        Ok(view_ops::Versions {
            before,
            after,
            from: from.unwrap_or_default(),
            to: to.unwrap_or_else(|| "now".into()),
        })
    }

    fn export_svg(&mut self, art: &Artwork, revision: &str) -> Result<ActionOutput, String> {
        let text = self.look_text.get_or_insert_with(TextRenderer::new);
        let renderer = &mut self.renderer;
        let svg = crate::svg::export(art, |layer| {
            let options = Options {
                layers: Some(vec![layer.id.clone()]),
                bare: true,
                ..Options::default()
            };
            view_ops::render(renderer, text, art, (art.bounds(), 1.0), &options, false)
                .ok()
                .map(|picture| picture.bytes)
        });
        let address = crate::resource::store(svg.as_bytes())?;
        let mut value = json!({
            "format": "svg",
            "bytes": svg.len(),
            "content": address,
            "revision": revision,
        });
        if svg.len() <= MAX_INLINE_SVG
            && let Value::Object(fields) = &mut value
        {
            fields.insert("svg".into(), svg.into());
        }
        Ok(ActionOutput {
            value: Some(value),
            resources: vec![address],
        })
    }

    /// The reference picture as `inspect` reports it, when there is one.
    fn reference_info(&mut self, art: &Artwork) -> Option<Value> {
        let reference = art.reference.as_ref()?;
        let frame = self.renderer.reference_frame(art);
        let pixels = self
            .renderer
            .reference_image(art)
            .map(|image| [image.width, image.height]);
        Some(json!({
            "image": reference.image,
            "frame": frame.map(|frame| [frame.x0, frame.y0, frame.width(), frame.height()]),
            "pixels": pixels,
            "opacity": reference.opacity,
            "visible": reference.visible,
        }))
    }

    fn answer(&mut self, op: &ViewOp, doc: &mut AutoCommit) -> Result<ActionOutput, String> {
        let version = match op {
            ViewOp::Look(look) => look.version.as_deref(),
            ViewOp::Inspect(inspect) => inspect.version.as_deref(),
            _ => None,
        };
        let (art, revision, shown) = Self::at_version(doc, version)?;
        let stamp = |info: &mut Value| {
            if let Value::Object(fields) = info {
                fields.insert("revision".into(), revision.clone().into());
                if let Some(shown) = &shown {
                    fields.insert("version".into(), shown.clone().into());
                }
            }
        };
        let picture = |result: Result<(Option<view_ops::Picture>, Value), String>| -> Result<ActionOutput, String> {
            let (picture, mut info) = result?;
            stamp(&mut info);
            let Some(picture) = picture else {
                return Ok(ActionOutput {
                    value: Some(info),
                    resources: Vec::new(),
                });
            };
            let hash = tilo_content::ContentStore::for_app()
                .put(&picture.bytes)
                .map_err(|error| format!("the picture could not be stored: {error}"))?;
            let address = tilo_actions::content_resource(&hash)
                .ok_or_else(|| "the picture has no content address".to_owned())?;
            if let Value::Object(fields) = &mut info {
                fields.insert("content".into(), address.clone().into());
            }
            Ok(ActionOutput {
                value: Some(info),
                resources: vec![address],
            })
        };
        let some = |result: Result<(view_ops::Picture, Value), String>| {
            result.map(|(picture, info)| (Some(picture), info))
        };
        let text = self.look_text.get_or_insert_with(TextRenderer::new);
        match op {
            ViewOp::Look(look) => {
                picture(some(view_ops::look(&mut self.renderer, text, &art, look)))
            }
            ViewOp::Export(export) => picture(some(view_ops::export(
                &mut self.renderer,
                text,
                &art,
                export,
            ))),
            ViewOp::Diff(diff) => {
                let versions = Self::versions(doc, &art, diff)?;
                let look = diff.look.unwrap_or(false);
                picture(view_ops::diff(&mut self.renderer, text, &versions, look))
            }
            ViewOp::Compare(compare) => picture(some(view_ops::compare(
                &mut self.renderer,
                text,
                &art,
                compare,
            ))),
            ViewOp::ExportSvg => self.export_svg(&art, &revision),
            ViewOp::Inspect(inspect) => {
                let mut value = view_ops::inspect(&art, inspect)?;
                stamp(&mut value);
                if let Value::Object(fields) = &mut value
                    && shown.is_none()
                {
                    fields.insert("selected".into(), json!(self.selection));
                    if let Some(reference) = self.reference_info(&art) {
                        fields.insert("reference".into(), reference);
                    }
                }
                Ok(ActionOutput {
                    value: Some(value),
                    resources: Vec::new(),
                })
            }
            ViewOp::Media => Ok(ActionOutput {
                value: Some(view_ops::media(&art)),
                resources: Vec::new(),
            }),
        }
    }

    /// What the pointer hovers over, outlined so a person sees what a click
    /// would pick.
    fn hovered(&self) -> Option<&Mark> {
        if self.drag.is_some()
            || self.text.is_some()
            || self.points.is_some()
            || self.current_tool() != Tool::Select
            || !self.rect.contains(self.cursor)
        {
            return None;
        }
        self.art
            .mark_at(self.content_point(self.cursor))
            .filter(|mark| !self.selection.contains(&mark.id))
    }

    fn paint_overlay(
        &self,
        scene: &mut Scene,
        to_screen: Affine,
        theme: &Theme,
        text: &mut TextRenderer,
    ) {
        self.overlay_selection(scene, to_screen, theme);
        self.overlay_drag(scene, to_screen, theme, text);
        self.overlay_paths(scene, to_screen, theme);
        if let Some(session) = &self.text {
            self.draw_caret(scene, to_screen, theme, text, session);
        }
    }

    /// What is hovered and picked: outlines, the frame and its guides.
    fn overlay_selection(&self, scene: &mut Scene, to_screen: Affine, theme: &Theme) {
        let accent = theme.accent_primary;
        let zoom = self.zoom();
        let hair = Stroke::new(1.0);
        let line = Stroke::new(1.5);
        let screen = |mut path: BezPath, affine: Affine| {
            path.apply_affine(to_screen * affine);
            path
        };
        let outline = |mark: &Mark| -> BezPath {
            match &mark.shape {
                Shape::Text { .. } => mark
                    .shape
                    .text_box()
                    .map(|area| ui::vello::kurbo::Shape::to_path(&area, 0.1))
                    .unwrap_or_default(),
                shape => shape.path(),
            }
        };
        if let Some(mark) = self.hovered() {
            scene.stroke(
                &line,
                Affine::IDENTITY,
                accent,
                None,
                &screen(outline(mark), mark.affine()),
            );
        }
        let preview = match &self.drag {
            Some(Drag::Move { affine, .. } | Drag::Handle { affine, .. }) => *affine,
            _ => Affine::IDENTITY,
        };
        // Marks on painted layers are not re-simulated while dragged: their
        // outline travels in their place.
        if preview != Affine::IDENTITY {
            let ghost = accent.multiply_alpha(0.7);
            for mark in self
                .selection
                .iter()
                .filter_map(|id| self.art.mark(id))
                .filter(|mark| self.mark_painted(mark))
            {
                let moved = mark.transformed(preview);
                scene.stroke(
                    &line,
                    Affine::IDENTITY,
                    ghost,
                    None,
                    &screen(outline(&moved), moved.affine()),
                );
            }
        }
        // Several things picked: each (a group as one) gets its own box
        // inside the frame around them all.
        let mut units: Vec<(&str, Rect)> = Vec::new();
        for mark in self.selection.iter().filter_map(|id| self.art.mark(id)) {
            let unit = if mark.group.is_empty() {
                mark.id.as_str()
            } else {
                mark.group.as_str()
            };
            let bounds = mark.transformed(preview).bounds();
            match units.iter_mut().find(|(id, _)| *id == unit) {
                Some((_, area)) => *area = area.union(bounds),
                None => units.push((unit, bounds)),
            }
        }
        if units.len() > 1 && self.points.is_none() {
            for (_, bounds) in &units {
                scene.stroke(
                    &hair,
                    Affine::IDENTITY,
                    accent.multiply_alpha(0.6),
                    None,
                    &screen(
                        ui::vello::kurbo::Shape::to_path(bounds, 0.1),
                        Affine::IDENTITY,
                    ),
                );
            }
        }
        if let Some(frame) = self.frame() {
            frame.draw_moved(scene, to_screen, preview, zoom, theme);
        }
        let guide = theme.tokens.colors.semantic.danger;
        for g in &self.guides {
            let (from, to) = if g.vertical {
                (Point::new(g.at, g.from), Point::new(g.at, g.to))
            } else {
                (Point::new(g.from, g.at), Point::new(g.to, g.at))
            };
            scene.stroke(
                &hair,
                Affine::IDENTITY,
                guide,
                None,
                &Line::new(to_screen * from, to_screen * to),
            );
        }
    }

    /// What a drag is making: a marquee, a shape, a stroke on a plain layer.
    fn overlay_drag(
        &self,
        scene: &mut Scene,
        to_screen: Affine,
        theme: &Theme,
        text: &mut TextRenderer,
    ) {
        let accent = theme.accent_primary;
        let hair = Stroke::new(1.0);
        match &self.drag {
            Some(Drag::Marquee { start, now, .. }) => {
                let area = Rect::from_points(to_screen * *start, to_screen * *now);
                scene.fill(
                    Fill::NonZero,
                    Affine::IDENTITY,
                    accent.multiply_alpha(0.08),
                    None,
                    &area,
                );
                scene.stroke(&hair, Affine::IDENTITY, accent, None, &area);
            }
            Some(Drag::Shape { start, now }) => {
                let colour = self.tools.colour.with(|colour| *colour);
                if self.current_tool() == Tool::Line {
                    let end = self.line_end(*start, *now);
                    let pen = Stroke::new(self.ink().width).with_caps(ui::vello::kurbo::Cap::Round);
                    scene.stroke(&pen, to_screen, colour, None, &Line::new(*start, end));
                } else {
                    let rect = self.shape_rect(*start, *now);
                    let ghost = colour.multiply_alpha(0.6);
                    if self.current_tool() == Tool::Ellipse {
                        let ellipse = ui::vello::kurbo::Ellipse::from_rect(rect);
                        scene.fill(Fill::NonZero, to_screen, ghost, None, &ellipse);
                    } else {
                        scene.fill(Fill::NonZero, to_screen, ghost, None, &rect);
                    }
                    let edge = Rect::from_points(
                        to_screen * rect.origin(),
                        to_screen * Point::new(rect.x1, rect.y1),
                    );
                    scene.stroke(&hair, Affine::IDENTITY, accent, None, &edge);
                }
            }
            Some(Drag::Stroke { draft, ink, .. }) if !self.painted(ink.layer.as_deref()) => {
                let colour = self.tools.colour.with(|colour| *colour);
                let points = draft.preview();
                let preview = Mark {
                    id: String::new(),
                    layer: String::new(),
                    order: String::new(),
                    name: String::new(),
                    shape: Shape::Stroke {
                        points,
                        size: ink.width,
                    },
                    style: crate::model::Style {
                        stroke: Some(hex(colour)),
                        ..crate::model::Style::default()
                    },
                    transform: None,
                    group: String::new(),
                    clip: String::new(),
                };
                crate::render::draw_vector(&preview, &[], scene, to_screen, text);
            }
            _ => {}
        }
    }

    /// The path the pen is laying down, or the points of one being edited.
    fn overlay_paths(&self, scene: &mut Scene, to_screen: Affine, theme: &Theme) {
        let accent = theme.accent_primary;
        let line = Stroke::new(1.5);
        let screen = |mut path: BezPath, affine: Affine| {
            path.apply_affine(to_screen * affine);
            path
        };
        if let Some(pen) = &self.pen {
            let cursor = (!pen.pulling).then(|| self.content_point(self.cursor));
            let cursor = match (
                cursor,
                self.modifiers.shift_key(),
                pen.contour.anchors.last(),
            ) {
                (Some(cursor), true, Some(last)) => Some(constrain_45(last.at, cursor)),
                (cursor, ..) => cursor,
            };
            let path = pen.preview(cursor);
            if pen.contour.closed {
                let fill = self.tools.colour.with(|colour| colour.multiply_alpha(0.35));
                scene.fill(Fill::NonZero, to_screen, fill, None, &path);
            }
            scene.stroke(
                &line,
                Affine::IDENTITY,
                accent,
                None,
                &screen(path, Affine::IDENTITY),
            );
            let last = pen.contour.anchors.len().saturating_sub(1);
            for (index, anchor) in pen.contour.anchors.iter().enumerate() {
                let handles = index == last || (pen.contour.closed && index == 0);
                draw_anchor(scene, (to_screen, theme), anchor, false, handles);
            }
        }
        if let Some(points) = &self.points {
            let affine = points.affine;
            scene.stroke(
                &line,
                Affine::IDENTITY,
                accent,
                None,
                &screen(points.outline.to_path(), affine),
            );
            for (point, anchor) in points.outline.anchors() {
                let active = points.active.is_some_and(|active| {
                    active.contour == point.contour && active.anchor == point.anchor
                });
                let moved = crate::gesture::outline::Anchor {
                    at: affine * anchor.at,
                    before: anchor.before.map(|at| affine * at),
                    after: anchor.after.map(|at| affine * at),
                };
                draw_anchor(scene, (to_screen, theme), &moved, active, active);
            }
        }
    }

    /// The typing frame and caret of a text being typed.
    fn draw_caret(
        &self,
        scene: &mut Scene,
        to_screen: Affine,
        theme: &Theme,
        text: &mut TextRenderer,
        session: &TextSession,
    ) {
        let Shape::Text {
            x,
            y,
            text: words,
            size,
            line_height,
            align,
            ..
        } = &session.shape
        else {
            return;
        };
        let affine = self
            .art
            .mark(&session.id)
            .map_or(Affine::IDENTITY, Mark::affine);
        let Some(style) = crate::render::text_style(&session.shape) else {
            return;
        };
        let caret = session.caret.caret().min(words.len());
        let before = words.get(..caret).unwrap_or_default();
        let row = before.matches('\n').count();
        let line_start = before.rfind('\n').map_or(0, |at| at + 1);
        let line_end = words
            .get(caret..)
            .and_then(|rest| rest.find('\n'))
            .map_or(words.len(), |at| caret + at);
        let measure = |text: &mut TextRenderer, part: &str| {
            if part.is_empty() {
                0.0
            } else {
                f64::from(text.measure_style(part, None, &style).0)
            }
        };
        let prefix = measure(text, words.get(line_start..caret).unwrap_or_default());
        let whole = measure(text, words.get(line_start..line_end).unwrap_or_default());
        let left = match align.as_deref() {
            Some("center") => x - whole / 2.0,
            Some("end") => x - whole,
            _ => *x,
        };
        let leading = size * line_height.unwrap_or(1.2);
        #[allow(clippy::cast_precision_loss, reason = "a text has at most 2000 lines")]
        let top = y - size * 0.8 + leading * row as f64;
        let accent = theme.accent_primary;
        let at = to_screen * affine;
        let caret_line = Line::new(
            at * Point::new(left + prefix, top),
            at * Point::new(left + prefix, top + size * 1.05),
        );
        scene.stroke(
            &Stroke::new(theme.caret_width.max(1.5)),
            Affine::IDENTITY,
            accent,
            None,
            &caret_line,
        );
        if let Some(area) = session.shape.text_box() {
            let pad = self.units(4.0);
            let mut frame = ui::vello::kurbo::Shape::to_path(&area.inflate(pad, pad), 0.1);
            frame.apply_affine(at);
            scene.stroke(
                &Stroke::new(1.0).with_dashes(0.0, [4.0, 3.0]),
                Affine::IDENTITY,
                accent.multiply_alpha(0.8),
                None,
                &frame,
            );
        }
    }
}

/// An anchor of a path being drawn or edited, and its handles when
/// `handles`; `active` fills it.
fn draw_anchor(
    scene: &mut Scene,
    (to_screen, theme): (Affine, &Theme),
    anchor: &crate::gesture::outline::Anchor,
    active: bool,
    handles: bool,
) {
    let accent = theme.accent_primary;
    let surface = theme.tokens.colors.surfaces.base;
    let line = Stroke::new(1.0);
    let at = to_screen * anchor.at;
    if handles {
        for handle in [anchor.before, anchor.after].into_iter().flatten() {
            let handle = to_screen * handle;
            scene.stroke(
                &line,
                Affine::IDENTITY,
                accent,
                None,
                &Line::new(at, handle),
            );
            let knob = Circle::new(handle, 3.5);
            scene.fill(Fill::NonZero, Affine::IDENTITY, surface, None, &knob);
            scene.stroke(&line, Affine::IDENTITY, accent, None, &knob);
        }
    }
    let square = Rect::from_center_size(at, (7.0, 7.0));
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        if active { accent } else { surface },
        None,
        &square,
    );
    scene.stroke(&line, Affine::IDENTITY, accent, None, &square);
}

impl Widget for CanvasSurface {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        use ui::widget::TaffyTreeExt as _;
        self.metrics = theme.tokens.metrics;
        self.spacing = theme.tokens.spacing;
        // Room for a selection frame's handles and the rotation handle.
        let least = taffy::Dimension::length(self.metrics.min_pointer_target * 4.0);
        let node = taffy.leaf_node(Style {
            size: taffy::Size {
                width: taffy::Dimension::auto(),
                height: taffy::Dimension::percent(1.0),
            },
            flex_basis: taffy::Dimension::length(0.0),
            flex_grow: 1.0,
            flex_shrink: 1.0,
            min_size: taffy::Size {
                width: least,
                height: least,
            },
            ..Default::default()
        });
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).copied().unwrap_or_default();
        let x = origin.x + f64::from(layout.location.x);
        let y = origin.y + f64::from(layout.location.y);
        self.rect = Rect::new(
            x,
            y,
            x + f64::from(layout.size.width),
            y + f64::from(layout.size.height),
        );
        self.viewport.set_rect(self.rect);
        self.viewport
            .set_content_size(self.art.width, self.art.height);
        let sizes = (
            self.rect.width(),
            self.rect.height(),
            self.art.width,
            self.art.height,
        );
        if !self.moved_by_hand && self.fitted_to != Some(sizes) && self.rect.width() > 1.0 {
            self.viewport.reset_to_fit();
            self.fitted_to = Some(sizes);
        }
        if self.rect.width() > 1.0
            && let Some(area) = self.staged_view.take()
        {
            self.viewport.zoom_to_rect(area);
            self.moved_by_hand = true;
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        self.follow_tool();
        scene.push_layer(
            Fill::NonZero,
            ui::vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &self.rect,
        );
        scene.fill(Fill::NonZero, transform, theme.bg_app, None, &self.rect);
        let to_screen = transform * self.viewport.content_to_screen();
        let board = self.art.bounds();
        let zoom = self.zoom();
        let shadow = theme.border_standard.multiply_alpha(0.5);
        scene.fill(
            Fill::NonZero,
            to_screen,
            shadow,
            None,
            &(board + Vec2::new(3.0 / zoom, 4.0 / zoom)),
        );
        let options = Options {
            reference: self
                .art
                .reference
                .as_ref()
                .is_some_and(|reference| reference.visible),
            wet: self.wet(),
            ..Options::default()
        };
        if let Some(shown) = self.shown() {
            self.renderer
                .draw(&shown, scene, to_screen, text_renderer, &options);
        } else {
            let art = Rc::clone(&self.art);
            self.renderer
                .draw(&art, scene, to_screen, text_renderer, &options);
        }
        scene.stroke(
            &Stroke::new(1.0 / zoom),
            to_screen,
            theme.border_standard,
            None,
            &board,
        );
        self.paint_overlay(scene, to_screen, theme, text_renderer);
        if self.focused {
            scene.stroke(
                &Stroke::new(2.0),
                transform,
                theme.border_focus,
                None,
                &self.rect.inset(-1.0),
            );
        }
        scene.pop_layer();
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        match event {
            WindowEvent::ModifiersChanged(modifiers) => {
                self.modifiers = modifiers.state();
                self.update_drag();
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                if self.drag.is_some() {
                    self.drag_to(self.cursor);
                    *ctx.handled = true;
                }
            }
            WindowEvent::MouseInput { state, button, .. } => match (state, button) {
                (ElementState::Pressed, MouseButton::Left) if self.rect.contains(self.cursor) => {
                    *ctx.focus_target = Some(ctx.node_id);
                    self.modifiers = ctx.modifiers.state();
                    self.press(self.cursor);
                    *ctx.handled = true;
                }
                (ElementState::Pressed, MouseButton::Middle) if self.rect.contains(self.cursor) => {
                    self.viewport.begin_pan(self.cursor);
                    self.drag = Some(Drag::Pan);
                    *ctx.handled = true;
                }
                (ElementState::Released, MouseButton::Left | MouseButton::Middle)
                    if self.drag.is_some() =>
                {
                    self.release();
                    *ctx.handled = true;
                }
                _ => {}
            },
            WindowEvent::MouseWheel { delta, .. } if self.rect.contains(self.cursor) => {
                if self.viewport.on_wheel(
                    delta,
                    &ctx.modifiers.state(),
                    self.cursor,
                    ctx.scale_factor,
                ) {
                    self.moved_by_hand = true;
                    *ctx.handled = true;
                }
            }
            WindowEvent::PinchGesture { delta, .. } if self.rect.contains(self.cursor) => {
                self.zoom_by((1.0 + delta).max(0.1), Some(self.cursor));
                *ctx.handled = true;
            }
            WindowEvent::Ime(ui::winit::event::Ime::Commit(typed)) if self.text.is_some() => {
                if let Some(session) = &mut self.text {
                    session.insert(typed);
                }
                *ctx.handled = true;
            }
            WindowEvent::KeyboardInput { event, .. } if self.focused => {
                if event.logical_key == Key::Named(NamedKey::Space) && self.text.is_none() {
                    self.space = event.state == ElementState::Pressed;
                    *ctx.handled = true;
                } else if event.state == ElementState::Pressed
                    && self.key(
                        &event.logical_key,
                        event.text.as_deref(),
                        ctx.modifiers.state(),
                    )
                {
                    *ctx.handled = true;
                }
            }
            _ => {}
        }
    }

    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[VIEW_TYPE]
    }

    fn start_operation(
        &mut self,
        op: &ResourceOperation<'_>,
        doc: &mut AutoCommit,
        _path: &ObjId,
    ) -> Result<Option<ResourceOperationStart>, String> {
        let Some(request) = op.unscoped() else {
            return Ok(None);
        };
        if request.type_id != VIEW_TYPE {
            return Ok(None);
        }
        let view: ViewOp = serde_json::from_slice(request.payload)
            .map_err(|error| format!("not a canvas view: {error}"))?;
        let output = self.answer(&view, doc)?;
        Ok(Some(ResourceOperationStart::Completed(output)))
    }

    fn poll_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        _cancellation_requested: bool,
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Option<InvocationUpdate> {
        self.results.remove(&invocation)
    }

    fn accessibility(&self) -> Option<AccessNode> {
        let layers = self.art.layers.len();
        let marks = self.art.marks.len();
        Some(
            AccessNode::new(Role::Image)
                .with_name(if self.art.title.is_empty() {
                    "Artwork".to_owned()
                } else {
                    self.art.title.clone()
                })
                .with_description(format!(
                    "{} by {} units, {layers} layers, {marks} marks, {} selected; inspect lists them and look shows them",
                    self.art.width,
                    self.art.height,
                    self.selection.len()
                )),
        )
    }

    fn is_focusable(&self) -> bool {
        true
    }

    fn set_focus(&mut self, focused: bool) {
        self.focused = focused;
        if !focused {
            self.space = false;
        }
    }

    fn is_focused(&self) -> bool {
        self.focused
    }

    fn ime_purpose(&self) -> Option<ui::winit::window::ImePurpose> {
        self.text
            .is_some()
            .then_some(ui::winit::window::ImePurpose::Normal)
    }

    fn needs_animation_frame(&self) -> bool {
        // A slow medium previews the stroke behind the pointer: keep
        // drawing until it has caught up.
        self.wet().is_some()
    }

    fn cursor_shape(&self, point: Point) -> Option<CursorIcon> {
        if !self.rect.contains(point) {
            return None;
        }
        if matches!(self.drag, Some(Drag::Pan)) {
            return Some(CursorIcon::Grabbing);
        }
        if self.space {
            return Some(CursorIcon::Grab);
        }
        let at = self.content_point(point);
        let tool = self.current_tool();
        if let Some(Drag::Handle { handle, frame, .. }) = &self.drag {
            return Some(handle.cursor(frame.angle));
        }
        if tool == Tool::Select
            && let Some(frame) = self.frame()
            && let Some(handle) = frame.handle_at(at, self.zoom())
        {
            return Some(handle.cursor(frame.angle));
        }
        Some(match tool {
            _ if self.text.is_some() => CursorIcon::Text,
            Tool::Select if matches!(self.drag, Some(Drag::Move { .. })) => CursorIcon::Move,
            Tool::Select => CursorIcon::Default,
            Tool::Text => CursorIcon::Text,
            Tool::Eyedropper
            | Tool::Pen
            | Tool::Brush
            | Tool::Rect
            | Tool::Ellipse
            | Tool::Line => CursorIcon::Crosshair,
        })
    }

    fn sync(&mut self, _state: &mut AppModel, _path: &ModelPath) {}

    fn name(&self) -> &'static str {
        "CanvasSurface"
    }

    fn test_id(&self) -> Option<&str> {
        Some("canvas")
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

impl ResourceSurface<ArtworkResource> for CanvasSurface {
    fn present(&mut self, state: &Rc<Artwork>) {
        self.expected.retain(|id| state.mark(id).is_none());
        let expected = &self.expected;
        self.selection
            .retain(|id| state.mark(id).is_some() || expected.contains(id));
        if let Some(points) = &mut self.points {
            match state.mark(&points.id) {
                None => self.points = None,
                Some(mark) => {
                    if mark.shape != points.shape
                        && !matches!(self.drag, Some(Drag::Point { .. }))
                        && let Some(outline) = Outline::of_shape(&mark.shape)
                    {
                        points.outline = outline;
                        points.shape = mark.shape.clone();
                        points.active = None;
                    }
                    points.affine = mark.affine();
                }
            }
        }
        if let Some(session) = &self.text
            && !session.new
            && state.mark(&session.id).is_none()
        {
            self.text = None;
        }
        self.art = Rc::clone(state);
    }

    fn attach(&mut self, cx: SurfaceCx<ArtworkResource>) {
        self.read_only = cx.read_only();
        self.dispatcher = Some(cx.dispatcher());
    }

    fn read_only_changed(&mut self, read_only: bool) {
        self.read_only = read_only;
    }

    /// The screenshots show the canvas mid-work: a selection with its
    /// handles, the points of a path, a close look at the paint.
    fn stage(&mut self, scenario: Option<&str>) {
        let select = |surface: &mut Self| {
            surface.tools.tool.set(Tool::Select);
            surface.tool = Tool::Select;
        };
        match scenario {
            Some("landscape") => self.selection = vec!["sun".into()],
            Some("close-up") => {
                self.selection = vec!["sun".into()];
                self.staged_view = Some(Rect::new(700.0, 120.0, 1000.0, 320.0));
            }
            Some("selection") => {
                select(self);
                self.selection = edit::with_groups(&self.art, &["heart".into()]);
            }
            Some("turned") => {
                select(self);
                self.selection = vec!["tilted".into()];
            }
            Some("points") => {
                select(self);
                self.start_points("leaf");
                if let Some(points) = &mut self.points {
                    points.active = Some(PointRef {
                        contour: 0,
                        anchor: 1,
                        part: Part::Anchor,
                    });
                }
                if let Some((bounds, _)) = edit::frame(&self.art, &["leaf".into()]) {
                    self.staged_view = Some(bounds.inflate(60.0, 60.0));
                }
            }
            _ => {}
        }
    }
}
