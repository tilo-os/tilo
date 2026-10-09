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
use ui::framework::{Dispatcher, ResourceSurface, SurfaceCx};
use ui::taffy::{self, NodeId, Style, TaffyTree};
use ui::tilo_actions::{self, ActionOutput};
use ui::vello::Scene;
use ui::vello::kurbo::{Affine, BezPath, Circle, Line, Point, Rect, Stroke, Vec2};
use ui::vello::peniko::Fill;
use ui::widget::{
    AppModel, EventCtx, ModelPath, PenSample, SelectionHandle, SelectionHandles, Widget,
};
use ui::winit::event::{ElementState, MouseButton, WindowEvent};
use ui::winit::keyboard::{Key, ModifiersState, NamedKey};
use ui::winit::window::CursorIcon;
use ui::{PresenceCtx, TextRenderer, Theme};

use crate::actions::{CanvasAction, Draw, EditMarks, NewMark, NewStroke, Paint, RemoveMarks};
use crate::gesture::area::{self, AreaGesture};
use crate::gesture::brush::BrushDraft;
use crate::gesture::outline::{Part, PenDraft, PointRef, constrain_45};
use crate::gesture::points::Editable;
use crate::gesture::snap::{Guide, Snap, snap_point, snap_rect};
use crate::gesture::text::{TextSession, Typed};
use crate::gesture::{Ink, edit};
use crate::model::{Artwork, Mark, Shape};
use crate::render::{Options, Renderer};
use crate::resource::{ArtworkResource, ExportSvg, ViewOp};
use crate::view_ops;

/// The largest SVG `export-svg` also answers inline, in bytes.
const MAX_INLINE_SVG: usize = 48 * 1024;
/// How big a shape made by a click without a drag is, in screen pixels.
const CLICKED_SHAPE: f64 = 100.0; // the size of a mark in the artwork, not on-screen spacing
/// The text size a new text starts at, in screen pixels.
const TEXT_SIZE: f64 = 32.0; // a size of lettering in the artwork, not on-screen spacing
/// How much one wheel notch of a pinch or a zoom key zooms.
const ZOOM_STEP: f64 = 1.25;

/// Whole-cell enlargement and reciprocal reductions; choosing down keeps a
/// fitted sheet wholly visible. Rounding near exact steps avoids float drift.
fn pixel_zoom(desired: f64, up: bool) -> f64 {
    let desired = desired.clamp(0.05, 64.0);
    if desired >= 1.0 {
        if up {
            (desired - 1e-9).ceil()
        } else {
            (desired + 1e-9).floor()
        }
    } else {
        let inverse = 1.0 / desired;
        1.0 / if up {
            (inverse + 1e-9).floor()
        } else {
            (inverse - 1e-9).ceil()
        }
    }
}

pub use crate::tools::{Tool, Tools, ViewRequest};

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
        /// The eraser's stroke, taking paint off rather than laying it.
        erase: bool,
        /// The stroke this one carries on from its end.
        extends: Option<String>,
    },
    /// The stroke eraser dragged across strokes, taking each it crosses.
    Sweep {
        path: Vec<Point>,
        crossed: Vec<String>,
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
    /// An area being selected ([`AreaGesture`] holds it).
    Area,
}

/// A path's points being edited: its outline in the mark's own coordinates.
struct PointEdit {
    id: String,
    shape: Shape,
    affine: Affine,
    outline: Editable,
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
    /// Mouse-wheel notches use the shared motion driver; trackpad input
    /// follows the hand directly. This is view state, never document history.
    zoom_motion: Option<(ui::animation::Animated<f64>, Point)>,
    zoom_duration: ui::time::Duration,
    motion_reduced: bool,
    /// Shapes text for looks and exports, which run outside a frame.
    look_text: Option<TextRenderer>,
    pictures: crate::background::Pictures,
    finding: bool,
    tools: Tools,
    /// The tool the modes below belong to; a change of tool ends them.
    tool: Tool,
    drag: Option<Drag>,
    selection: Vec<String>,
    /// Marks just made or pasted, selected before the artwork has them.
    expected: Vec<String>,
    pen: Option<PenDraft>,
    /// An area being selected.
    area: AreaGesture,
    points: Option<PointEdit>,
    text: Option<TextSession>,
    /// Where the text caret was last drawn, for the input method's
    /// candidate window.
    ime_caret: std::cell::Cell<Option<Rect>>,
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
    /// Focus came from a press on the canvas, not the keyboard: the
    /// pointer already says where the person is, so no ring is drawn.
    pressed_into_focus: bool,
    space: bool,
    modifiers: ModifiersState,
    /// What the pen reported with the pointer's newest move or press; none
    /// when a mouse moved it.
    stylus: Option<PenSample>,
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
            // The artboard sits in the middle of the room it is given.
            viewport: ui::widget::ViewportState::new()
                .with_zoom_bounds(0.05, 64.0)
                .with_alignment(
                    ui::widget::SmallContentAlign::Center,
                    ui::widget::SmallContentAlign::Center,
                ),
            fitted_to: None,
            moved_by_hand: false,
            zoom_motion: None,
            zoom_duration: ui::tokens::Durations::default().fast(),
            motion_reduced: false,
            look_text: None,
            pictures: crate::background::Pictures::default(),
            finding: false,
            tools,
            tool,
            drag: None,
            selection: Vec::new(),
            expected: Vec::new(),
            pen: None,
            area: AreaGesture::default(),
            points: None,
            text: None,
            ime_caret: std::cell::Cell::new(None),
            guides: Vec::new(),
            cursor: Point::ZERO,
            last_press: None,
            epoch: ui::time::now(),
            pastes: 0,
            staged_view: None,
            focused: false,
            pressed_into_focus: false,
            space: false,
            modifiers: ModifiersState::empty(),
            stylus: None,
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

    /// How far off a line or stroke a click still picks it, in artwork
    /// units: a few screen pixels, whatever the zoom.
    fn pick_slop(&self) -> f64 {
        self.units(f64::from(self.spacing.s1))
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
        self.area.cancel();
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
        let Some(outline) = Editable::of_mark(mark) else {
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

    /// Enter a mark: edit a path's points or a text's words, crop a
    /// picture.
    fn enter(&mut self, id: &str) -> bool {
        if self.read_only {
            return false;
        }
        if self
            .art
            .mark(id)
            .is_some_and(|mark| matches!(mark.shape, Shape::Image { .. }))
        {
            self.select(vec![id.to_owned()]);
            self.tools.crop.set(true);
            return true;
        }
        self.start_text_edit(id) || self.start_points(id)
    }

    /// A picture the chooser answered with: the artwork, when the sheet is
    /// empty; else placed on it, fitted inside and selected.
    fn take_picked(&mut self) {
        let answer = {
            let picking = self.tools.picking.borrow();
            let Some(channel) = picking.as_ref() else {
                return;
            };
            match channel.try_recv() {
                Err(std::sync::mpsc::TryRecvError::Empty) => return,
                Ok(answer) => Some(answer),
                Err(std::sync::mpsc::TryRecvError::Disconnected) => None,
            }
        };
        *self.tools.picking.borrow_mut() = None;
        // A chooser that could not answer, or a file that is no picture,
        // is said beside the title, as Files' answers are: never silence.
        let notice = self.tools.notice.clone();
        let said = move |why: String| {
            notice.set(Some(ui::i18n::I18nManager::format(
                "canvas-picture-failed",
                &[("why", why.into())],
            )));
        };
        let files = match answer {
            Some(ui::portal::ContentsOutcome::Picked(files)) => files,
            Some(ui::portal::ContentsOutcome::Cancelled) => return,
            Some(ui::portal::ContentsOutcome::Failed(why)) => return said(why),
            None => return said(ui::i18n::I18nManager::lookup_plain("canvas-chooser-gone")),
        };
        let Some(file) = files.into_iter().next() else {
            return;
        };
        if self.art.marks.is_empty() {
            match crate::resource::open_picture(&file.name, &file.bytes) {
                Ok(open) => {
                    self.dispatch(&open);
                    self.select_made(vec!["picture".into()]);
                    self.tools.tool.set(Tool::Select);
                }
                Err(error) => said(error),
            }
            return;
        }
        match crate::resource::keep_picture_bytes(&file.name, &file.bytes) {
            Ok((src, size)) => self.place_picture(src, size),
            Err(error) => said(error),
        }
    }

    /// The picture at `src` placed on the artwork: as large as fits inside
    /// it at a unit to a pixel or less, in its middle, selected.
    fn place_picture(&mut self, src: String, (width, height): (u32, u32)) {
        let (width, height) = (f64::from(width), f64::from(height));
        let fit = (self.art.width * 0.8 / width)
            .min(self.art.height * 0.8 / height)
            .min(1.0);
        let (w, h) = (width * fit, height * fit);
        let shape = Shape::Image {
            src,
            x: ((self.art.width - w) / 2.0).round(),
            y: ((self.art.height - h) / 2.0).round(),
            width: w.round(),
            height: h.round(),
            crop: None,
            straighten: None,
        };
        let id = edit::fresh_id(&self.art, "picture", &[]);
        let ink = self.ink();
        self.dispatch(&CanvasAction::Draw(Draw {
            layer: ink.layer,
            marks: vec![NewMark {
                id: Some(id.clone()),
                ..NewMark::new(shape)
            }],
        }));
        self.select_made(vec![id]);
        self.tools.tool.set(Tool::Select);
    }

    /// The picture being cropped: the one image selected in crop mode.
    fn cropping(&self) -> Option<&Mark> {
        if !self.tools.crop.with(|on| *on) {
            return None;
        }
        match self.selection.as_slice() {
            [id] => self
                .art
                .mark(id)
                .filter(|mark| matches!(mark.shape, Shape::Image { .. })),
            _ => None,
        }
    }

    /// Crop mode ends when its picture is no longer the one selected.
    fn follow_crop(&self) {
        if self.cropping().is_none() && self.tools.crop.with(|on| *on) {
            self.tools.crop.set(false);
        }
    }

    /// A picture's shape after a handle drag in crop mode, `affine` in
    /// artwork units: an edge or corner crops it, the picture staying put;
    /// the turn handle straightens it.
    fn cropped(mark: &Mark, handle: SelectionHandle, affine: Affine) -> Shape {
        let mut shape = mark.shape.clone();
        let Some(placement) = crate::picture::Placement::of(&shape) else {
            return shape;
        };
        let placement = if handle == SelectionHandle::Rotate {
            let [a, b, ..] = affine.as_coeffs();
            let turn = b.atan2(a).to_degrees();
            crate::picture::Placement {
                straighten: ((placement.straighten + turn) * 10.0)
                    .round()
                    .clamp(-450.0, 450.0)
                    / 10.0,
                ..placement
            }
        } else {
            let local = mark.affine().inverse() * affine * mark.affine();
            placement.recropped(local.transform_rect_bbox(placement.frame))
        };
        placement.apply(&mut shape);
        shape
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
                ..RemoveMarks::default()
            }));
            return;
        }
        let shape = points.outline.to_shape(&points.shape);
        if shape != points.shape {
            let action = CanvasAction::edit(EditMarks {
                ids: vec![points.id.clone()],
                shape: Some(crate::actions::Taught(shape.clone())),
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
                && now.duration_since(then) <= ui::gesture::multi_click_interval()
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
            Tool::Brush | Tool::Eraser => Some(self.press_brush(at, tool)),
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
                self.pick_colour(at);
                None
            }
            Tool::Marquee | Tool::Oval => {
                self.area.press(at, Some(tool == Tool::Oval));
                Some(Drag::Area)
            }
            Tool::Lasso => {
                if double && !self.area.polygon.is_empty() {
                    let mode = self.area_mode();
                    if let Some(action) = self.area.finish(mode) {
                        self.dispatch(&action);
                    }
                    return;
                }
                self.area.press(at, None);
                Some(Drag::Area)
            }
            Tool::Wand | Tool::Bucket => {
                self.find_colour(at, tool == Tool::Bucket);
                None
            }
        };
    }

    /// The eyedropper's click: the brush takes the colour the canvas shows
    /// at `at`, after masks and erasers, as the eye sees it.
    fn pick_colour(&mut self, at: Point) {
        if !self.art.bounds().contains(at) {
            return;
        }
        let text = self.look_text.get_or_insert_with(TextRenderer::new);
        let shown =
            view_ops::shown_colour(&mut self.tools.renderer.borrow_mut(), text, &self.art, at);
        // A canvas that cannot draw a picture of itself picks nothing.
        if let Ok(colour) = shown {
            self.tools.pick(colour);
        }
    }

    /// How a new area meets the selection, from the held keys.
    fn area_mode(&self) -> Option<String> {
        area::mode(self.modifiers.shift_key(), self.modifiers.alt_key())
    }

    /// The magic wand's or the fill bucket's click: like colour touching
    /// `at`, as the canvas shows it, selected or filled.
    fn find_colour(&mut self, at: Point, fill: bool) {
        if !self.art.bounds().contains(at) {
            return;
        }
        // The bucket fills inside the selection, and the wand taking from
        // it or keeping part of it looks only there: what it finds cannot
        // reach other parts of it through colour outside.
        let mode = self.area_mode();
        let within = (fill || matches!(mode.as_deref(), Some("subtract" | "intersect")))
            .then(|| {
                self.art
                    .selection
                    .as_ref()
                    .map(crate::model::Selection::region)
            })
            .flatten();
        // Sampling one layer, the chosen one (the top one when none is):
        // the bucket's own choice, or the wand's.
        let sampling = if fill {
            &self.tools.fill_sampling
        } else {
            &self.tools.sampling
        };
        let layers = (sampling.with(|sampling| *sampling) == crate::tools::Sampling::Layer)
            .then(|| {
                self.ink()
                    .layer
                    .or_else(|| self.art.top_layer().map(|layer| layer.id.clone()))
            })
            .flatten()
            .map(|layer| vec![layer]);
        let query = crate::author::ColourQuery {
            at: Some(at),
            tolerance: self.tools.tolerance.with(|tolerance| *tolerance),
            // Ctrl finds the colour everywhere, touching or not.
            everywhere: !fill && self.modifiers.control_key(),
            layers,
            within,
            ..crate::author::ColourQuery::default()
        };
        let text = self.look_text.get_or_insert_with(TextRenderer::new);
        let found = crate::author::colour_region(
            &mut self.tools.renderer.borrow_mut(),
            text,
            &self.art,
            &query,
        );
        // A canvas that cannot draw a picture of itself finds nothing.
        let Ok(found) = found else {
            return;
        };
        let action = if fill {
            area::bucket(
                &self.art,
                &found,
                self.ink(),
                edit::fresh_id(&self.art, "fill", &[]),
            )
        } else {
            Some(area::found(&found, mode))
        };
        if let Some(action) = action {
            self.dispatch(&action);
        }
    }

    /// The brush or eraser lands at `at`: a stroke, carried on from the
    /// selected stroke when it lands on its end; or the stroke eraser.
    fn press_brush(&self, at: Point, tool: Tool) -> Drag {
        let at = if self.tools.preset.with(|preset| preset.pixel()) {
            Point::new((at.x + 1e-9).floor(), (at.y + 1e-9).floor())
        } else {
            at
        };
        let erase = tool == Tool::Eraser || self.stylus.is_some_and(|stylus| stylus.eraser);
        let ink = self.ink();
        if erase && self.tools.erasing.with(|erasing| *erasing) == crate::tools::Erasing::Strokes {
            let crossed = crate::strokes::crossed(&self.art, &[[at.x, at.y]], ink.layer.as_deref());
            return Drag::Sweep {
                path: vec![at],
                crossed,
            };
        }
        let pressure = self.pressure();
        Drag::Stroke {
            draft: BrushDraft::new(at, pressure),
            id: edit::fresh_id(&self.art, "s", &[]),
            extends: (!erase).then(|| self.extendable(at)).flatten(),
            ink,
            erase,
        }
    }

    /// The selected stroke whose end `at` lands on, to carry it on.
    fn extendable(&self, at: Point) -> Option<String> {
        let [id] = self.selection.as_slice() else {
            return None;
        };
        let mark = self.art.mark(id)?;
        let Shape::Stroke { points, size, .. } = &mark.shape else {
            return None;
        };
        if mark.erases() || mark.transform.is_some() || self.mark_painted(mark) {
            return None;
        }
        let reach = (size * 0.5).max(self.units(f64::from(self.metrics.min_pointer_target) / 2.0));
        let end = |point: Option<&Vec<f64>>| {
            point.is_some_and(|point| {
                Point::new(
                    point.first().copied().unwrap_or(0.0),
                    point.get(1).copied().unwrap_or(0.0),
                )
                .distance(at)
                    <= reach
            })
        };
        (end(points.first()) || end(points.last())).then(|| id.clone())
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
            if alt
                && point.part == Part::Anchor
                && let Some(cut) = points.outline.stroke_point(point)
            {
                // Alt on a stroke's point cuts the stroke in two there.
                let at = points.affine * cut;
                let id = points.id.clone();
                self.points = None;
                self.dispatch(&CanvasAction::edit(EditMarks {
                    ids: vec![id],
                    split: Some([at.x, at.y]),
                    ..EditMarks::default()
                }));
                return true;
            }
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
        let hit = edit::pick_near(&self.art, at, self.pick_slop(), inside_group);
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
        let pressure = self.pressure();
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
            Some(Drag::Stroke { draft, .. }) => {
                let at = if self.tools.preset.with(|preset| preset.pixel()) {
                    Point::new((at.x + 1e-9).floor(), (at.y + 1e-9).floor())
                } else {
                    at
                };
                draft.push(at, seconds, zoom, pressure);
            }
            Some(Drag::Sweep { path, crossed }) => {
                let from = path.last().copied().unwrap_or(at);
                path.push(at);
                let layer = self.tools.ink().layer;
                for id in crate::strokes::crossed(
                    &self.art,
                    &[[from.x, from.y], [at.x, at.y]],
                    layer
                        .as_deref()
                        .filter(|layer| self.art.layer(layer).is_some()),
                ) {
                    if !crossed.contains(&id) {
                        crossed.push(id);
                    }
                }
            }
            Some(Drag::Area) => self.area.drag_to(at, 2.0 / zoom),
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

    /// The pressure the pen behind the pointer reports, when it reports
    /// one: a pen without a pressure sensor, like a mouse, leaves the hand's
    /// speed to stand in for it.
    fn pressure(&self) -> Option<f64> {
        self.stylus
            .map(|pen| pen.pressure)
            .filter(|pressure| *pressure > 0.0)
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
                handle,
                ..
            } => {
                if start.distance(now) * zoom >= 1.0 {
                    self.release_handle(handle, ids, affine);
                }
            }
            drag @ (Drag::Stroke { .. } | Drag::Sweep { .. }) => self.finish_brush(drag),
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
            Drag::Area => self.release_area(),
        }
    }

    /// An area tool let go: the area it selected, if any.
    fn release_area(&mut self) {
        let slop = self.units(self.click_slop());
        let mode = self.area_mode();
        if let Some(action) = self.area.release(slop, mode) {
            self.dispatch(&action);
        }
    }

    /// A handle let go: the marks stretched or turned, or in crop mode the
    /// picture cropped or straightened.
    fn release_handle(&self, handle: SelectionHandle, ids: Vec<String>, affine: Affine) {
        let Some(mark) = self.cropping() else {
            if let Some(action) = edit::transform(ids, affine) {
                self.dispatch(&action);
            }
            return;
        };
        let shape = Self::cropped(mark, handle, affine);
        let Some(placement) = crate::picture::Placement::of(&shape) else {
            return;
        };
        let edit = if handle == SelectionHandle::Rotate {
            EditMarks {
                ids,
                straighten: Some(placement.straighten),
                ..EditMarks::default()
            }
        } else {
            EditMarks {
                ids,
                crop: Some(placement.crop.to_vec()),
                ..EditMarks::default()
            }
        };
        self.dispatch(&CanvasAction::edit(edit));
    }

    /// The brush or eraser lifts: its stroke is laid, carried on from the
    /// stroke it extends, or the strokes it swept are taken away.
    fn finish_brush(&mut self, drag: Drag) {
        match drag {
            Drag::Stroke {
                draft,
                id,
                ink,
                erase,
                extends,
            } => {
                let points = draft.finish();
                if let Some(extended) = extends {
                    self.dispatch(&CanvasAction::edit(EditMarks {
                        ids: vec![extended],
                        extend: Some(points),
                        ..EditMarks::default()
                    }));
                    return;
                }
                let stroke = self.new_stroke(id, points, &ink, erase);
                self.dispatch(&CanvasAction::Paint(Paint {
                    layer: ink.layer,
                    strokes: vec![stroke],
                    group: None,
                }));
            }
            Drag::Sweep { crossed, .. } if !crossed.is_empty() => {
                self.selection.retain(|id| !crossed.contains(id));
                self.dispatch(&CanvasAction::RemoveMarks(RemoveMarks {
                    ids: crossed,
                    ..RemoveMarks::default()
                }));
            }
            _ => {}
        }
    }

    /// The stroke the brush or eraser laid, as `paint` takes it.
    fn new_stroke(&self, id: String, points: Vec<Vec<f64>>, ink: &Ink, erase: bool) -> NewStroke {
        let layer = ink
            .layer
            .clone()
            .or_else(|| self.art.layers.last().map(|layer| layer.id.clone()))
            .unwrap_or_default();
        // Painting and erasing keep inside the selection while there is one.
        let clip = self
            .art
            .selection
            .is_some()
            .then(|| crate::actions::SELECTION_CLIP.to_owned());
        if erase {
            let hardness = self.tools.eraser_hardness.with(|value| *value);
            let strength = self.tools.eraser_strength.with(|value| *value);
            return NewStroke {
                id: Some(id),
                points,
                size: ink.width,
                brush: Some(
                    if self.tools.preset.with(|preset| preset.pixel()) {
                        "pixel-eraser"
                    } else {
                        "eraser"
                    }
                    .into(),
                ),
                clip,
                opacity: (strength < 1.0).then_some((strength * 100.0).round() / 100.0),
                params: (hardness < 0.995).then(|| {
                    std::collections::BTreeMap::from([(
                        "hardness".to_owned(),
                        (hardness * 100.0).round() / 100.0,
                    )])
                }),
                ..NewStroke::default()
            };
        }
        let opacity = self.tools.opacity.with(|value| *value);
        let brush = self.tools.brush_name(&self.art, &layer);
        // A broad nib is held at the angle the pen leans, when one does.
        let angle = (brush.as_deref() == Some("calligraphy"))
            .then(|| {
                self.stylus
                    .filter(|stylus| stylus.tilt.0.hypot(stylus.tilt.1) > 5.0)
                    .map(|stylus| (stylus.tilt.1.atan2(stylus.tilt.0).to_degrees() + 90.0).round())
            })
            .flatten();
        NewStroke {
            id: Some(id),
            name: None,
            points,
            size: ink.width,
            colour: ink.colour.clone(),
            opacity: (opacity < 1.0).then_some((opacity * 100.0).round() / 100.0),
            wet: Some(true),
            params: self.tools.stroke_params(&self.art, &layer),
            brush,
            angle,
            clip,
            ..NewStroke::default()
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
        self.zoom_motion = None;
        let old = self.zoom();
        let target = if self.tools.preset.with(|preset| preset.pixel()) {
            pixel_zoom(old * factor, factor > 1.0)
        } else {
            old * factor
        };
        match focal {
            Some(focal) => self.viewport.zoom_at(target / old, focal),
            None => self.viewport.zoom_by(target / old),
        }
        self.moved_by_hand = true;
    }

    fn wheel(
        &mut self,
        delta: &ui::winit::event::MouseScrollDelta,
        modifiers: ModifiersState,
        scale: f64,
    ) -> bool {
        if let ui::winit::event::MouseScrollDelta::LineDelta(_, y) = delta
            && (modifiers.control_key() || modifiers.super_key())
            && !self.motion_reduced
            && *y != 0.0
        {
            self.advance_zoom();
            let raw = f64::from(*y) * 56.0;
            let factor = if raw > 0.0 {
                1.0 + raw / 240.0
            } else {
                1.0 / (1.0 - raw / 240.0)
            };
            let from = self.zoom().ln();
            let target = self
                .zoom_motion
                .as_ref()
                .map_or_else(|| self.zoom(), |(motion, _)| motion.to().exp());
            let (min, max) = self.viewport.zoom_bounds();
            let target = (target * factor).clamp(min, max).ln();
            self.zoom_motion = Some((
                ui::animation::Animated::new(
                    from,
                    target,
                    self.zoom_duration,
                    ui::animation::Curve::EaseOut,
                ),
                self.cursor,
            ));
            self.moved_by_hand = true;
            return true;
        }
        self.zoom_motion = None;
        if !self
            .viewport
            .on_wheel(delta, &modifiers, self.cursor, scale)
        {
            return false;
        }
        self.moved_by_hand = true;
        true
    }

    fn zoom_to_fit(&mut self) {
        self.zoom_motion = None;
        self.viewport.reset_to_fit();
        self.moved_by_hand = false;
    }

    fn zoom_to_selection(&mut self) {
        self.zoom_motion = None;
        if let Some((bounds, _)) = edit::frame(&self.art, &self.selection) {
            let margin = bounds.width().max(bounds.height()) * 0.1;
            self.viewport.zoom_to_rect(bounds.inflate(margin, margin));
            self.moved_by_hand = true;
        }
    }

    /// Carry out a change of view asked for by the zoom buttons, and tell
    /// them how far the canvas is zoomed now.
    /// Carry out the canvas's keys pressed while something else had the
    /// focus.
    fn follow_keys(&mut self) {
        if let Some(ids) = self.tools.pick_objects.with(Clone::clone) {
            self.tools.pick_objects.set(None);
            self.selection = ids
                .into_iter()
                .filter(|id| self.art.mark(id).is_some())
                .collect();
            self.tools.tool.set(Tool::Select);
            self.tools.selection.set(self.selection.clone());
        }
        if self.tools.keys.with(Vec::is_empty) {
            return;
        }
        let keys = self.tools.keys.with(Clone::clone);
        self.tools.keys.set(Vec::new());
        for pressed in keys {
            self.key(&pressed.key, pressed.text.as_deref(), pressed.modifiers);
        }
    }

    fn follow_view(&mut self) {
        if self.rect.width() > 1.0
            && let Some(request) = self.tools.view.with(|view| *view)
        {
            self.zoom_motion = None;
            self.tools.view.set(None);
            match request {
                ViewRequest::ZoomIn => self.zoom_by(ZOOM_STEP, None),
                ViewRequest::ZoomOut => self.zoom_by(1.0 / ZOOM_STEP, None),
                ViewRequest::Fit => self.zoom_to_fit(),
                ViewRequest::Pixel => {
                    self.viewport.set_zoom(pixel_zoom(self.zoom(), false));
                    self.moved_by_hand = true;
                }
                ViewRequest::Actual => {
                    self.zoom_motion = None;
                    self.viewport.set_zoom(1.0);
                    self.moved_by_hand = true;
                }
            }
        }
        self.follow_width();
        if self.tools.preset.with(|preset| preset.pixel()) && !self.moved_by_hand {
            self.viewport.set_zoom(pixel_zoom(self.zoom(), false));
        }
        if self.tools.preset.with(|preset| preset.pixel())
            && (self.zoom() - self.zoom().round()).abs() < 1e-9
        {
            let origin = self.viewport.content_point_to_screen(Point::ORIGIN);
            self.viewport
                .pan_by(origin.x - origin.x.round(), origin.y - origin.y.round());
        }
        let zoom = self.zoom();
        if self.tools.zoom.with(|shown| (*shown - zoom).abs() > 1e-9) {
            // The readout is drawn before the canvas: wake the window so it
            // shows the new zoom in the next frame.
            self.tools.zoom.writer().set(zoom);
        }
        // What the canvas shows, for exporting just that.
        let shown = Rect::from_points(
            self.content_point(self.rect.origin()),
            self.content_point(Point::new(self.rect.x1, self.rect.y1)),
        );
        let moved = self.tools.visible.with(|visible| {
            visible.is_none_or(|visible| {
                (visible.x0 - shown.x0).abs()
                    + (visible.y0 - shown.y0).abs()
                    + (visible.x1 - shown.x1).abs()
                    + (visible.y1 - shown.y1).abs()
                    > 0.5
            })
        });
        if self.rect.width() > 1.0 && moved {
            self.tools.visible.set(Some(shown));
        }
    }

    fn advance_zoom(&mut self) {
        let Some((motion, focal)) = self.zoom_motion else {
            return;
        };
        let now = ui::time::now();
        self.viewport
            .zoom_at(motion.current(now).exp() / self.zoom(), focal);
        if motion.done(now) {
            self.zoom_motion = None;
        }
    }

    /// Tell the strip over the canvas how much room it has. The window
    /// shell docks the inspector, or makes it a drawer, as the window's
    /// width asks.
    fn follow_width(&self) {
        if self.rect.width() <= 1.0 {
            return;
        }
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a laid-out width, positive and far below u32::MAX"
        )]
        let room =
            (self.rect.width().max(0.0) as u32) / crate::tools::ROOM_STEP * crate::tools::ROOM_STEP;
        if self.tools.room.with(|shown| *shown != room) {
            self.tools.room.writer().set(room);
        }
    }

    /// The view keys: zoom in and out, to fit, to 100 %, to the selection,
    /// and the overview of the keys.
    fn view_key(&mut self, key: &Key, modifiers: ModifiersState) -> bool {
        let Key::Character(ch) = key else {
            return false;
        };
        let command = modifiers.control_key() || modifiers.super_key();
        match (ch.as_str(), command, modifiers.shift_key()) {
            // The keys, which a keyboard may report as Shift and slash.
            ("?", false, _) | ("/", false, true) => {
                return crate::panels::files::window_key(&self.tools, "?", modifiers);
            }
            ("+" | "=", _, _) => self.zoom_by(ZOOM_STEP, None),
            ("-" | "_", _, _) => self.zoom_by(1.0 / ZOOM_STEP, None),
            ("0", true, _) | ("!" | "1", false, true) => self.zoom_to_fit(),
            ("1", true, _) | ("0" | ")", false, true) => {
                self.zoom_motion = None;
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
        if self.area_key(key) {
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
        if self.crop_key(key, modifiers) {
            return true;
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
                self.dispatch(&CanvasAction::RemoveMarks(RemoveMarks {
                    ids,
                    ..RemoveMarks::default()
                }));
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
            Key::Character(ch) if self.tools.size_key(ch) => true,
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

    /// C crops the selected picture or ends cropping; Enter and Escape end
    /// it too.
    fn crop_key(&mut self, key: &Key, modifiers: ModifiersState) -> bool {
        let cropping = self.cropping().is_some();
        match key {
            Key::Named(NamedKey::Enter | NamedKey::Escape) if cropping => {
                self.tools.crop.set(false);
                true
            }
            Key::Character(ch) if ch.as_str() == "c" && !modifiers.alt_key() => {
                if cropping {
                    self.tools.crop.set(false);
                    return true;
                }
                let [id] = self.selection.as_slice() else {
                    return false;
                };
                let id = id.clone();
                let picture = self
                    .art
                    .mark(&id)
                    .is_some_and(|mark| matches!(mark.shape, Shape::Image { .. }));
                picture && self.enter(&id)
            }
            _ => false,
        }
    }

    /// Keys for the selected area: Enter ends a lasso being clicked out,
    /// Escape drops it or the selection, Delete clears the selection from
    /// the layer. Marks picked on the canvas come first.
    fn area_key(&mut self, key: &Key) -> bool {
        let Key::Named(named) = key else {
            return false;
        };
        if !self.area.polygon.is_empty() {
            match named {
                NamedKey::Enter => {
                    let mode = self.area_mode();
                    if let Some(action) = self.area.finish(mode) {
                        self.dispatch(&action);
                    }
                    return true;
                }
                NamedKey::Escape => {
                    self.area.cancel();
                    return true;
                }
                _ => {}
            }
        }
        if !self.selection.is_empty() || self.art.selection.is_none() {
            return false;
        }
        match named {
            NamedKey::Escape => self.dispatch(&area::none()),
            NamedKey::Delete | NamedKey::Backspace => {
                self.dispatch(&CanvasAction::UseSelection(
                    crate::selection::UseSelection {
                        what: "clear".into(),
                        layer: self.ink().layer,
                        ..crate::selection::UseSelection::default()
                    },
                ));
            }
            _ => return false,
        }
        true
    }

    /// Ctrl/Cmd with a letter: the clipboard, select all, duplicate, group,
    /// and opening, saving and exporting the artwork.
    fn command_key(&mut self, ch: &str, shift: bool) -> bool {
        let offset = Vec2::new(
            self.units(self.copy_offset()),
            self.units(self.copy_offset()),
        );
        let mut held = ModifiersState::CONTROL;
        held.set(ModifiersState::SHIFT, shift);
        if crate::panels::files::window_key(&self.tools, ch, held) {
            return true;
        }
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
                    self.dispatch(&CanvasAction::RemoveMarks(RemoveMarks {
                        ids,
                        ..RemoveMarks::default()
                    }));
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
            ("d", false) if self.selection.is_empty() && self.art.selection.is_some() => {
                self.dispatch(&area::none());
            }
            ("i", true) if self.art.selection.is_some() => {
                self.dispatch(&CanvasAction::Select(Box::new(crate::selection::Select {
                    invert: Some(true),
                    ..crate::selection::Select::default()
                })));
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
        let cropping = self.cropping().map(|mark| mark.id.clone());
        match &self.drag {
            Some(Drag::Handle {
                ids,
                affine,
                handle,
                ..
            }) if *affine != Affine::IDENTITY && cropping.is_some() => {
                let art = shown.get_or_insert_with(|| (*self.art).clone());
                for mark in &mut art.marks {
                    if ids.contains(&mark.id) {
                        mark.shape = Self::cropped(mark, *handle, *affine);
                    }
                }
            }
            Some(Drag::Stroke {
                draft,
                extends: Some(id),
                ..
            }) => {
                let art = shown.get_or_insert_with(|| (*self.art).clone());
                if let Some(mark) = art.marks.iter_mut().find(|mark| mark.id == *id)
                    && let Shape::Stroke { points, .. } = &mut mark.shape
                {
                    crate::strokes::extend(points, &draft.preview());
                }
            }
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
        let live: Vec<crate::tools::Live> = self.tools.live.borrow().clone();
        for control in live {
            let Some(value) = control.moved() else {
                continue;
            };
            let art = shown.get_or_insert_with(|| (*self.art).clone());
            (control.apply)(art, value);
        }
        if let Some(text) = &self.text {
            let art = shown.get_or_insert_with(|| (*self.art).clone());
            if let Some(mark) = art.marks.iter_mut().find(|mark| mark.id == text.id) {
                mark.shape = text.shown_shape();
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
                    shape: text.shown_shape(),
                    style: crate::model::Style {
                        fill: Some(text.fill.clone()),
                        ..crate::model::Style::default()
                    },
                    transform: None,
                    group: String::new(),
                    clip: String::new(),
                    area: String::new(),
                });
            }
        }
        shown
    }

    /// The stroke being laid, shown on its layer as it will be drawn or
    /// painted: an eraser's clears what is under it.
    fn wet(&self) -> Option<Mark> {
        let Some(Drag::Stroke {
            draft,
            id,
            ink,
            erase,
            extends: None,
        }) = &self.drag
        else {
            return None;
        };
        let stroke = self.new_stroke(id.clone(), draft.preview(), ink, *erase);
        let layer = ink
            .layer
            .clone()
            .or_else(|| self.art.layers.last().map(|layer| layer.id.clone()))?;
        let (clip, area) =
            crate::actions::clip_named(&self.art, stroke.clip.as_deref()).unwrap_or_default();
        Some(Mark {
            id: id.clone(),
            layer,
            order: String::new(),
            name: String::new(),
            shape: Shape::plain_stroke(stroke.points, stroke.size),
            style: crate::model::Style {
                stroke: (!stroke.colour.is_empty()).then_some(stroke.colour),
                brush: stroke.brush,
                opacity: stroke.opacity,
                params: stroke.params.unwrap_or_default(),
                angle: stroke.angle,
                wet: true,
                ..crate::model::Style::default()
            },
            transform: None,
            group: String::new(),
            clip,
            area,
        })
    }

    /// The revision a view answers about: the document's heads.
    fn revision(heads: &[ui::automerge::ChangeHash]) -> String {
        let mut hasher = blake3::Hasher::new();
        for head in heads {
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
        cx: &Reading,
        version: Option<&str>,
    ) -> Result<(Artwork, String, Option<String>), String> {
        let Some(version) = version else {
            let document = cx
                .document()
                .ok_or_else(|| "the artwork is not open here".to_owned())?;
            return Ok((
                crate::doc::read(&document),
                Self::revision(&cx.heads()),
                None,
            ));
        };
        let past = cx.version(version)?;
        Ok((
            crate::doc::read(&past.doc),
            Self::revision(&past.heads),
            Some(past.name),
        ))
    }

    /// The two versions a diff compares; `to` is now when absent.
    fn versions(
        cx: &Reading,
        now: &Artwork,
        diff: &crate::resource::Diff,
    ) -> Result<view_ops::Versions, String> {
        let (before, _, from) = Self::at_version(cx, Some(&diff.from))?;
        let (after, to) = match &diff.to {
            Some(to) => {
                let (after, _, to) = Self::at_version(cx, Some(to))?;
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

    fn export_svg(
        &mut self,
        art: &Artwork,
        revision: &str,
        export: &ExportSvg,
    ) -> Result<ActionOutput, String> {
        let part = match &export.ids {
            Some(ids) => Some(view_ops::part(art, ids)?),
            None => None,
        };
        let ground = part.is_none();
        let art = part.as_ref().unwrap_or(art);
        let text = self.look_text.get_or_insert_with(TextRenderer::new);
        let svg = crate::panels::export::svg(
            &mut self.tools.renderer.borrow_mut(),
            text,
            art,
            ground,
            export.outline_text == Some(true),
        );
        let address = crate::resource::store(svg.as_bytes())?;
        let mut value = json!({
            "format": "svg",
            "bytes": svg.len(),
            "content": address,
            "revision": revision,
        });
        let written = match &export.path {
            Some(path) => Some(crate::document::export_to(path, &["svg"], svg.as_bytes())?),
            None => None,
        };
        if let (Some(written), Value::Object(fields)) = (&written, &mut value) {
            fields.insert("path".into(), written.clone().into());
        }
        // Written to a file, the markup need not be read again.
        if written.is_none()
            && svg.len() <= MAX_INLINE_SVG
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
    fn reference_info(&self, art: &Artwork) -> Option<Value> {
        let reference = art.reference.as_ref()?;
        let mut renderer = self.tools.renderer.borrow_mut();
        let frame = renderer.reference_frame(art);
        let pixels = renderer
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

    /// The invocation sees one immutable revision, even when the person edits
    /// while its settled picture is being rendered.
    fn picture_job(&self, op: ViewOp, cx: &mut Reading) -> ui::framework::EffectAnswer {
        let version = match &op {
            ViewOp::Look(look) => look.version.as_deref(),
            _ => None,
        };
        let (art, revision, shown) = Self::at_version(cx, version)?;
        let path = match &op {
            ViewOp::Export(export) => export.path.clone(),
            _ => None,
        };
        let pictures = self.pictures.clone();
        let content = crate::content::store();
        let output_store = content.clone();
        let phase = ui::i18n::I18nManager::lookup_plain("canvas-rendering-artwork");
        Ok(cx.thread(
            move |progress| {
                progress.report(phase, None, None);
                pictures.render(&art, content, &op)
            },
            move |_, result, _| {
                let (picture, mut info) = result?;
                if let Some(path) = &path {
                    let extensions: &[&str] = match picture.format {
                        "jpeg" => &["jpg", "jpeg"],
                        "webp" => &["webp"],
                        _ => &["png"],
                    };
                    let written = crate::document::export_to(path, extensions, &picture.bytes)?;
                    if let Value::Object(fields) = &mut info {
                        fields.insert("path".into(), written.into());
                    }
                }
                Self::picture_output(&picture, info, &revision, shown.as_deref(), &output_store)
                    .map(ui::framework::Effected::Done)
            },
        ))
    }

    fn picture_output(
        picture: &view_ops::Picture,
        mut info: Value,
        revision: &str,
        shown: Option<&str>,
        content: &tilo_content::ContentStore,
    ) -> Result<ActionOutput, String> {
        let hash = content
            .put(&picture.bytes)
            .map_err(|error| format!("the picture could not be stored: {error}"))?;
        let address = tilo_actions::content_resource(&hash)
            .ok_or_else(|| "the picture has no content address".to_owned())?;
        if let Value::Object(fields) = &mut info {
            fields.insert("revision".into(), revision.into());
            fields.insert("content".into(), address.clone().into());
            if let Some(shown) = shown {
                fields.insert("version".into(), shown.into());
            }
        }
        Ok(ActionOutput {
            value: Some(info),
            resources: vec![address],
        })
    }

    /// The model and edge filter share the framework's progress, permission
    /// checks and attributed completion for buttons and semantic callers.
    fn area_job(
        &mut self,
        select: crate::selection::Select,
        cx: &mut Reading,
    ) -> ui::framework::EffectAnswer {
        if ui::rt::availability(ui::rt::Feature::Threads) != ui::rt::Availability::Available {
            return Err("image area finding needs background threads on this platform".into());
        }
        if self.finding {
            return Err(ui::i18n::I18nManager::lookup_plain("canvas-finding-busy"));
        }
        if select.subject != Some(true) && select.refine.is_none() {
            return Err(
                "give subject: true or refine: units; use select for geometric areas".into(),
            );
        }
        let (art, _, _) = Self::at_version(cx, None)?;
        let mut document = cx
            .document()
            .ok_or_else(|| "the artwork is not open here".to_owned())?
            .clone();
        if let Some(layer) = &select.layer
            && art.layer(layer).is_none_or(crate::model::Layer::is_mask)
        {
            return Err(format!("there is no picture layer {layer:?}"));
        }
        let pictures = self.pictures.clone();
        let content = crate::content::store();
        let sampled = art.clone();
        let request = select.clone();
        let phase = ui::i18n::I18nManager::lookup_plain(if select.subject == Some(true) {
            "canvas-finding-subject"
        } else {
            "canvas-refining-edge"
        });
        self.finding = true;
        self.tools.notice.set(Some(phase.clone()));
        Ok(cx.thread(
            move |progress| {
                progress.report(phase, None, None);
                pictures.prepare(&sampled, content, &request)?.finish()
            },
            move |surface, result, cx| {
                surface.finding = false;
                surface.tools.notice.set(None);
                let (area, matte, found) = result?;
                if cx.live().with(|current| current != &art) {
                    return Err(ui::i18n::I18nManager::lookup_plain("canvas-finding-changed"));
                }
                if cx.dispatcher().read_only() {
                    return Err(ui::i18n::I18nManager::lookup_plain("canvas-finding-read-only"));
                }
                let subject = select.subject == Some(true);
                let action = crate::author::found(select, area, matte);
                ui::tilo_actions::Action::apply(&action, &mut document).map_err(|error| error.to_string())?;
                let mut answer = json!({"selection": crate::answer::selection(&crate::doc::read(&document))});
                if subject && let Value::Object(fields) = &mut answer {
                    fields.insert("found_by".into(), json!({"model": found.model, "load_ms": found.load_ms, "infer_ms": found.infer_ms, "refine_ms": found.refine_ms}));
                }
                let CanvasAction::Select(select) = action else {
                    return Err("the finder did not produce a selection".into());
                };
                cx.invoke(ArtworkResource::SELECT_FOUND.with((select.d, select.matte, select.mode, select.feather)));
                Ok(ui::framework::Effected::value(answer))
            },
        ))
    }

    fn answer(&mut self, op: &ViewOp, cx: &Reading) -> Result<ActionOutput, String> {
        let version = match op {
            ViewOp::Look(look) => look.version.as_deref(),
            ViewOp::Inspect(inspect) => inspect.version.as_deref(),
            _ => None,
        };
        let (art, revision, shown) = Self::at_version(cx, version)?;
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
            let hash = crate::content::store()
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
            ViewOp::Look(look) => picture(some(view_ops::look(
                &mut self.tools.renderer.borrow_mut(),
                text,
                &art,
                look,
            ))),
            ViewOp::Export(export) => picture(some(view_ops::export(
                &mut self.tools.renderer.borrow_mut(),
                text,
                &art,
                export,
            ))),
            ViewOp::Diff(diff) => {
                let versions = Self::versions(cx, &art, diff)?;
                let look = diff.look.unwrap_or(false);
                picture(view_ops::diff(
                    &mut self.tools.renderer.borrow_mut(),
                    text,
                    &versions,
                    look,
                ))
            }
            ViewOp::Compare(compare) => picture(some(view_ops::compare(
                &mut self.tools.renderer.borrow_mut(),
                text,
                &art,
                compare,
            ))),
            ViewOp::ExportSvg(export) => self.export_svg(&art, &revision, export),
            ViewOp::Inspect(inspect) => {
                let mut value = view_ops::inspect(&art, inspect)?;
                if let Some(ids) = &inspect.measure {
                    let measured =
                        view_ops::measure(&mut self.tools.renderer.borrow_mut(), text, &art, ids)?;
                    if let Value::Object(fields) = &mut value {
                        fields.insert("measured".into(), measured);
                    }
                }
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
            ViewOp::SaveArtwork(save) => saved(crate::document::save(&art, save), &stamp),
            ViewOp::FindArea(_) => Err("area finding is handled as a job".into()),
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
            .pick_near(self.content_point(self.cursor), self.pick_slop())
            .filter(|mark| !self.selection.contains(&mark.id))
    }

    fn paint_overlay(
        &self,
        scene: &mut Scene,
        to_screen: Affine,
        theme: &Theme,
        text: &mut TextRenderer,
    ) {
        self.overlay_crop(scene, to_screen, theme);
        self.overlay_area(scene, to_screen);
        self.overlay_erasers(scene, to_screen, theme);
        self.overlay_selection(scene, to_screen, theme);
        self.overlay_drag(scene, to_screen, theme);
        self.overlay_paths(scene, to_screen, theme);
        self.overlay_footprint(scene, to_screen, theme);
        if let Some(session) = &self.text {
            self.draw_caret(scene, to_screen, theme, text, session);
        }
    }

    /// While a picture is cropped: the rest of the picture, faint, around
    /// its frame, and the picture's edge, as far as a crop can reach.
    fn overlay_crop(&self, scene: &mut Scene, to_screen: Affine, theme: &Theme) {
        let Some(mark) = self.cropping() else {
            return;
        };
        let shown;
        let mark = match self.shown() {
            Some(art) => {
                shown = art;
                shown.mark(&mark.id).unwrap_or(mark)
            }
            None => mark,
        };
        let (Shape::Image { src, .. }, Some(placement)) =
            (&mark.shape, crate::picture::Placement::of(&mark.shape))
        else {
            return;
        };
        let mut renderer = self.tools.renderer.borrow_mut();
        let Some(size) = renderer.pictures.size(src) else {
            return;
        };
        let at = to_screen * mark.affine();
        let to_local = placement.picture_to_local(size);
        let whole = to_local
            * ui::vello::kurbo::Shape::to_path(
                &Rect::new(0.0, 0.0, f64::from(size.0), f64::from(size.1)),
                0.1,
            );
        let device = at.determinant().abs().sqrt() / placement.pixels_per_unit(size).max(1e-9);
        if let Some((image, step)) =
            renderer
                .pictures
                .shown(src, device, &crate::picture::Treatment::default())
        {
            let mut outside = whole.clone();
            outside.extend(ui::vello::kurbo::Shape::path_elements(
                &placement.frame,
                0.1,
            ));
            // What the crop leaves out, dimmed under the theme's own
            // backdrop so the kept part stands out.
            scene.push_layer(
                Fill::EvenOdd,
                ui::vello::peniko::BlendMode::default(),
                1.0,
                at,
                &outside,
            );
            scene.fill(
                Fill::NonZero,
                at,
                &ui::vello::peniko::ImageBrush::new(image),
                Some(to_local * Affine::scale(step)),
                &whole,
            );
            scene.fill(
                Fill::NonZero,
                at,
                theme.tokens.colors.surfaces.content.multiply_alpha(0.6),
                None,
                &whole,
            );
            scene.pop_layer();
        }
        drop(renderer);
        let mut edge = whole;
        edge.apply_affine(at);
        let dashes = Stroke::new(1.0).with_dashes(0.0, [4.0, 4.0]);
        scene.stroke(
            &dashes,
            Affine::IDENTITY,
            theme.tokens.colors.intent.you,
            None,
            &edge,
        );
    }

    /// The selected area's outline, and the area being selected.
    fn overlay_area(&self, scene: &mut Scene, to_screen: Affine) {
        if let Some(path) = area::selection_path(&self.art, to_screen) {
            area::draw_outline(scene, &path, 0.0);
        }
        if let Some(mut path) = self.area.outline(self.content_point(self.cursor)) {
            path.apply_affine(to_screen);
            area::draw_outline(scene, &path, 0.0);
        }
    }

    /// Where erasers took paint off, outlined while the eraser or the
    /// selection tool is in hand: an eraser has no ink to see it by, and
    /// this is how a person finds one to pick, reshape or take away.
    fn overlay_erasers(&self, scene: &mut Scene, to_screen: Affine, theme: &Theme) {
        if !matches!(self.current_tool(), Tool::Select | Tool::Eraser) || self.text.is_some() {
            return;
        }
        let hovered = self.hovered().map(|mark| mark.id.as_str());
        for mark in self.art.erasers() {
            let mut outline = crate::render::silhouette(mark);
            outline.apply_affine(to_screen * mark.affine());
            let marked = hovered == Some(mark.id.as_str()) || self.selection.contains(&mark.id);
            ui::widget::selection_handles::ToolFeedback::Erased { marked }
                .draw(scene, &outline, theme);
        }
    }

    /// The brush's or eraser's footprint round the pointer: a ring as wide
    /// as the mark it lays at this zoom, dark and light so it reads on any
    /// paint, as a painter's hand expects to see where it will land.
    fn overlay_footprint(&self, scene: &mut Scene, to_screen: Affine, theme: &Theme) {
        if !matches!(self.current_tool(), Tool::Brush | Tool::Eraser)
            || self.space
            || matches!(self.drag, Some(Drag::Pan))
            || !self.rect.contains(self.cursor)
        {
            return;
        }
        let ring = Circle::new(
            self.content_point(self.cursor),
            self.tools.brush_size() / 2.0,
        );
        // Too small to read as a ring: the crosshair says where it lands.
        if ring.radius * self.zoom() < 2.0 {
            return;
        }
        let mut path = ui::vello::kurbo::Shape::to_path(&ring, 0.1);
        path.apply_affine(to_screen);
        ui::widget::selection_handles::ToolFeedback::Footprint.draw(scene, &path, theme);
    }

    /// What is hovered and picked: outlines, the frame and its guides.
    fn overlay_selection(&self, scene: &mut Scene, to_screen: Affine, theme: &Theme) {
        let accent = theme.tokens.colors.intent.you;
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
        if let Some(mark) = self.hovered().filter(|mark| !mark.erases()) {
            scene.stroke(
                &line,
                Affine::IDENTITY,
                accent,
                None,
                &screen(outline(mark), mark.affine()),
            );
        }
        let preview = match &self.drag {
            // Straightening turns the picture, not its frame.
            Some(Drag::Handle {
                handle: SelectionHandle::Rotate,
                ..
            }) if self.cropping().is_some() => Affine::IDENTITY,
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
    fn overlay_drag(&self, scene: &mut Scene, to_screen: Affine, theme: &Theme) {
        let accent = theme.tokens.colors.intent.you;
        let hair = Stroke::new(1.0);
        match &self.drag {
            Some(Drag::Marquee { start, now, .. }) => {
                let area = Rect::from_points(to_screen * *start, to_screen * *now);
                scene.fill(
                    Fill::NonZero,
                    Affine::IDENTITY,
                    theme.tokens.colors.intent.selection_tint,
                    None,
                    &area,
                );
                scene.stroke(&hair, Affine::IDENTITY, accent, None, &area);
            }
            Some(Drag::Shape { start, now }) => {
                let colour = self.tools.colour();
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
            Some(Drag::Sweep { path, crossed }) => {
                // The strokes the eraser has crossed, outlined, and its path.
                for mark in crossed.iter().filter_map(|id| self.art.mark(id)) {
                    let mut outline = crate::render::silhouette(mark);
                    outline.apply_affine(to_screen * mark.affine());
                    scene.fill(
                        Fill::NonZero,
                        Affine::IDENTITY,
                        theme.tokens.colors.semantic.danger.multiply_alpha(0.25),
                        None,
                        &outline,
                    );
                    scene.stroke(
                        &hair,
                        Affine::IDENTITY,
                        theme.tokens.colors.semantic.danger,
                        None,
                        &outline,
                    );
                }
                let mut trail = BezPath::new();
                for (index, point) in path.iter().enumerate() {
                    if index == 0 {
                        trail.move_to(to_screen * *point);
                    } else {
                        trail.line_to(to_screen * *point);
                    }
                }
                scene.stroke(
                    &Stroke::new(1.5),
                    Affine::IDENTITY,
                    accent.multiply_alpha(0.6),
                    None,
                    &trail,
                );
            }
            _ => {}
        }
    }

    /// The path the pen is laying down, or the points of one being edited.
    fn overlay_paths(&self, scene: &mut Scene, to_screen: Affine, theme: &Theme) {
        let accent = theme.tokens.colors.intent.you;
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
                let fill = self.tools.colour().multiply_alpha(0.35);
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
            size,
            line_height,
            align,
            ..
        } = &session.shape
        else {
            return;
        };
        // The caret and line are found in the text as shown, so a
        // composition in progress pushes the caret along with it.
        let words = session.shown_text();
        let words = words.as_ref();
        let affine = self
            .art
            .mark(&session.id)
            .map_or(Affine::IDENTITY, Mark::affine);
        let Some(style) = crate::render::text_style(&session.shape) else {
            return;
        };
        let caret = session.caret.display_caret(session.text()).min(words.len());
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
        let accent = theme.tokens.colors.intent.you;
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
        self.ime_caret
            .set(Some(ui::vello::kurbo::Shape::bounding_box(&caret_line)));
        // The composition is underlined until the input method commits it.
        if let Some(range) = session.caret.preedit_display_range(session.text())
            && range.start >= line_start
            && range.end <= line_end
        {
            let from = left + measure(text, words.get(line_start..range.start).unwrap_or_default());
            let to = left + measure(text, words.get(line_start..range.end).unwrap_or_default());
            let under = top + size * 1.05;
            scene.stroke(
                &Stroke::new(1.0),
                Affine::IDENTITY,
                accent,
                None,
                &Line::new(at * Point::new(from, under), at * Point::new(to, under)),
            );
        }
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
/// An image mark's picture as it shows in its frame, untransformed and
/// unclipped (the document does that), for an SVG to embed: cropped,
/// turned and graded, at the picture's own resolution; a JPEG when it is
/// opaque.
pub(crate) fn embedded_picture(
    renderer: &mut Renderer,
    text: &mut TextRenderer,
    art: &Artwork,
    mark: &Mark,
) -> Option<crate::svg::Embedded> {
    let Shape::Image { src, .. } = &mark.shape else {
        return None;
    };
    let placement = crate::picture::Placement::of(&mark.shape)?;
    let size = renderer.pictures.size(src)?;
    let frame = placement.frame;
    let longest = frame.width().max(frame.height()).max(1.0);
    let scale = placement
        .pixels_per_unit(size)
        .min(f64::from(crate::picture::MAX_PICTURE_SIDE) / longest);
    let mut layer = art.layer(&mark.layer)?.clone();
    layer.medium.clear();
    layer.opacity = 1.0;
    if let Some(adjust) = &mut layer.adjust {
        adjust.vignette = None;
    }
    let alone = Mark {
        transform: None,
        clip: String::new(),
        area: String::new(),
        style: crate::model::Style {
            adjust: mark.style.adjust.clone(),
            ..crate::model::Style::default()
        },
        ..mark.clone()
    };
    let solo = Artwork {
        layers: vec![layer],
        marks: vec![alone],
        ..art.clone()
    };
    let options = Options {
        bare: true,
        ..Options::default()
    };
    let (rgba, width, height) =
        view_ops::render_rgba(renderer, text, &solo, frame, scale, &options).ok()?;
    let opaque = rgba.chunks_exact(4).all(|pixel| pixel.get(3) == Some(&255));
    let (bytes, mime) = if opaque {
        (
            view_ops::encode_jpeg_at(&rgba, width, height, 92).ok()?,
            "image/jpeg",
        )
    } else {
        (
            view_ops::encode_png(&rgba, width, height).ok()?,
            "image/png",
        )
    };
    Some(crate::svg::Embedded { bytes, mime })
}

fn draw_anchor(
    scene: &mut Scene,
    (to_screen, theme): (Affine, &Theme),
    anchor: &crate::gesture::outline::Anchor,
    active: bool,
    handles: bool,
) {
    let accent = theme.tokens.colors.intent.you;
    let surface = theme.tokens.colors.surfaces.raised;
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
    fn set_motion_reduced(&mut self, reduced: bool) {
        self.motion_reduced = reduced;
        if reduced && let Some((motion, focal)) = self.zoom_motion.take() {
            self.viewport
                .zoom_at(motion.to().exp() / self.zoom(), focal);
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        use ui::widget::TaffyTreeExt as _;
        self.metrics = theme.tokens.metrics;
        self.spacing = theme.tokens.spacing;
        self.set_motion_reduced(theme.reduce_motion);
        self.zoom_duration = theme.tokens.motion.durations.fast();
        // Room for a selection frame's handles and the rotation handle.
        let least = taffy::Dimension::length(self.metrics.min_pointer_target * 4.0);
        // The room its parent leaves along either axis: in a column under
        // the header it takes the width and the height left over.
        let style = Style {
            size: taffy::Size {
                width: taffy::Dimension::percent(1.0),
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
        };
        taffy.widget_leaf(style, node_map, self)
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
        self.advance_zoom();
        self.follow_tool();
        self.follow_keys();
        self.follow_view();
        self.follow_crop();
        self.take_picked();
        scene.push_layer(
            Fill::NonZero,
            ui::vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &self.rect,
        );
        let to_screen = transform * self.viewport.content_to_screen();
        let board = self.art.bounds();
        // The sheet is an object lifted off the content ground: the one
        // soft shadow a floating thing casts, and an edge measured against
        // the ground it sits on.
        let sheet = self.viewport.content_to_screen().transform_rect_bbox(board);
        ui::surface::shadow(
            scene,
            transform,
            sheet,
            0.0,
            &theme.tokens.elevation.floating,
        );
        let options = Options {
            reference: self
                .art
                .reference
                .as_ref()
                .is_some_and(|reference| reference.visible),
            wet: self.wet(),
            detail: Some(crate::detail::Detail {
                visible: to_screen
                    .inverse()
                    .transform_rect_bbox(transform.transform_rect_bbox(self.rect))
                    .intersect(board),
                pixels_per_unit: to_screen.determinant().abs().sqrt(),
                wait: false,
            }),
            live: true,
            ..Options::default()
        };
        if let Some(shown) = self.shown() {
            self.tools.renderer.borrow_mut().draw(
                &shown,
                scene,
                to_screen,
                text_renderer,
                &options,
            );
        } else {
            let art = Rc::clone(&self.art);
            self.tools
                .renderer
                .borrow_mut()
                .draw(&art, scene, to_screen, text_renderer, &options);
        }
        ui::surface::ContentFrame::Sheet.draw(scene, transform, sheet, theme);
        self.paint_overlay(scene, to_screen, theme, text_renderer);
        if self.focused && !self.pressed_into_focus {
            ui::widget::paint_focus_ring(scene, transform, self.rect.inset(-2.0), 0.0, theme);
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
                self.stylus = ctx.pen();
                self.cursor =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                if self.drag.is_some() {
                    self.drag_to(self.cursor);
                    *ctx.handled = true;
                }
            }
            WindowEvent::MouseInput { state, button, .. } => match (state, button) {
                (ElementState::Pressed, MouseButton::Left) if self.rect.contains(self.cursor) => {
                    self.zoom_motion = None;
                    self.stylus = ctx.pen();
                    if !self.focused {
                        self.pressed_into_focus = true;
                    }
                    *ctx.focus_target = Some(ctx.node_id);
                    self.modifiers = ctx.modifiers.state();
                    self.press(self.cursor);
                    *ctx.handled = true;
                }
                // A right-click is about what is under it: with the
                // selecting tool it picks that mark first, as a left click
                // would, and the menu over the canvas offers what to do
                // with it. It leaves the press to that menu.
                (ElementState::Pressed, MouseButton::Right)
                    if self.rect.contains(self.cursor) && self.drag.is_none() =>
                {
                    if !self.focused {
                        self.pressed_into_focus = true;
                    }
                    *ctx.focus_target = Some(ctx.node_id);
                    if self.current_tool() == Tool::Select && self.points.is_none() {
                        let at = self.content_point(self.cursor);
                        let hit = edit::pick_near(&self.art, at, self.pick_slop(), false);
                        if hit.is_empty() {
                            if !self.frame().is_some_and(|frame| frame.contains(at)) {
                                self.select(Vec::new());
                            }
                        } else if !hit.iter().all(|id| self.selection.contains(id)) {
                            self.select(hit);
                        }
                        self.tools.selection.set(self.selection.clone());
                    }
                }
                (ElementState::Pressed, MouseButton::Middle) if self.rect.contains(self.cursor) => {
                    self.zoom_motion = None;
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
                if self.wheel(delta, ctx.modifiers.state(), ctx.scale_factor) {
                    *ctx.handled = true;
                }
            }
            WindowEvent::PinchGesture { delta, .. } if self.rect.contains(self.cursor) => {
                self.zoom_motion = None;
                self.viewport.zoom_at((1.0 + delta).max(0.1), self.cursor);
                self.moved_by_hand = true;
                *ctx.handled = true;
            }
            WindowEvent::Ime(ime) if self.text.is_some() => {
                if let Some(session) = &mut self.text {
                    session.ime(ime);
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
        if !focused || !self.focused {
            // Keeps the press's mark when the press is what focused it.
            self.pressed_into_focus &= focused;
        }
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

    fn ime_cursor_rect(&self) -> Option<Rect> {
        self.text
            .is_some()
            .then(|| self.ime_caret.get().unwrap_or(self.rect))
    }

    fn has_pending_visual_work(&self) -> bool {
        // Still captures must await the requested image and paint work;
        // asking for frames alone does not declare an unfinished placeholder.
        let renderer = self.tools.renderer.borrow();
        self.finding
            || renderer.pictures.decoding()
            || renderer.detail_pending()
            || renderer.settling()
    }

    fn needs_animation_frame(&self) -> bool {
        // A slow medium previews the stroke behind the pointer: keep
        // drawing until it has caught up. A picture being chosen, or
        // decoded, is waited for. Close up, the finer tiles of a painted
        // layer arrive as they are simulated, and a painted layer's
        // simulation catches up with a stroke just laid.
        self.zoom_motion.is_some()
            || self.wet().is_some()
            || self.tools.picking.borrow().is_some()
            || self.finding
            || self.tools.renderer.borrow().pictures.decoding()
            || self.tools.renderer.borrow().detail_pending()
            || self.tools.renderer.borrow().settling()
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
            | Tool::Eraser
            | Tool::Rect
            | Tool::Ellipse
            | Tool::Line
            | Tool::Marquee
            | Tool::Oval
            | Tool::Lasso
            | Tool::Wand
            | Tool::Bucket => CursorIcon::Crosshair,
        })
    }

    fn sync(&mut self, _state: &AppModel, _path: &ModelPath) {
        self.follow_keys();
        self.follow_view();
        // How large a brush can be follows the artwork's size.
        let extent = self.art.width.max(self.art.height);
        if extent > 0.0
            && self
                .tools
                .extent
                .with(|known| (*known - extent).abs() > f64::EPSILON)
        {
            self.tools.extent.set(extent);
        }
        // The panels show what is selected.
        if self.tools.selection.with(|shown| *shown != self.selection) {
            self.tools.selection.set(self.selection.clone());
        }
    }

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

/// What a canvas view reads the artwork through.
type Reading = ui::framework::EffectCx<ArtworkResource, CanvasSurface>;

impl ResourceSurface<ArtworkResource, ViewOp> for CanvasSurface {
    /// Settled pictures and image areas run as jobs; other views answer
    /// directly from the current artwork or the version the caller named.
    fn effect(
        &mut self,
        op: ViewOp,
        cx: &mut ui::framework::EffectCx<ArtworkResource, Self>,
    ) -> ui::framework::EffectAnswer {
        if matches!(op, ViewOp::Look(_) | ViewOp::Export(_))
            && ui::rt::availability(ui::rt::Feature::Threads) == ui::rt::Availability::Available
        {
            return self.picture_job(op, cx);
        }
        if let ViewOp::FindArea(select) = op {
            return self.area_job(select.into(), cx);
        }
        self.answer(&op, cx).map(ui::framework::Effected::Done)
    }

    fn present(&mut self, state: &Rc<Artwork>) {
        // A differently sized sheet needs its own fit; keeping a hand-panned
        // sprite view can otherwise put a newly opened photograph off screen.
        if self.art.width.to_bits() != state.width.to_bits()
            || self.art.height.to_bits() != state.height.to_bits()
        {
            self.moved_by_hand = false;
            self.fitted_to = None;
            self.zoom_motion = None;
        }
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
                        && let Some(outline) = Editable::of_mark(mark)
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
        // What the window shows is what the magic wand finds colour in.
        crate::author::see(&self.tools.renderer, &self.art);
    }

    fn attach(&mut self, cx: SurfaceCx<ArtworkResource>) {
        if let Some(content) = cx.content() {
            self.tools.renderer.borrow_mut().pictures.read_from(content);
        }
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

/// The answer of `save-artwork`: where the file went, or the content it
/// was kept as.
fn saved(
    result: Result<Value, String>,
    stamp: &dyn Fn(&mut Value),
) -> Result<ActionOutput, String> {
    let mut value = result?;
    stamp(&mut value);
    let resources = value
        .get("content")
        .and_then(Value::as_str)
        .map(|address| vec![address.to_owned()])
        .unwrap_or_default();
    Ok(ActionOutput {
        value: Some(value),
        resources,
    })
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        reason = "test assertions over exact, hand-written fixtures"
    )]

    use super::*;
    use crate::panels::shortcuts::{GROUPS, tools};

    #[test]
    fn capture_waits_for_requested_artwork_painting() {
        let mut doc = ui::tilo_resource::Doc::new();
        <ArtworkResource as ui::tilo_resource::Resource>::initialize(&mut doc).unwrap();
        for action in crate::sample::landscape() {
            ui::tilo_actions::Action::apply(&action, &mut doc).unwrap();
        }
        let art = crate::doc::read(&doc);
        assert!(art.layers.iter().any(|layer| !layer.medium.is_empty()));
        assert!(!art.marks.is_empty());
        let tools = Tools::default();
        let mut surface = CanvasSurface::new(tools.clone());
        surface.present(&Rc::new(art));
        let mut harness =
            ui::headless::FrameHarness::new(ui::State::new(), Box::new(surface), (390, 844));
        harness
            .wait_for_visuals(std::time::Duration::from_secs(30))
            .expect("the production capture wait completes");
        let renderer = tools.renderer.borrow();
        assert!(
            !renderer.settling() && !renderer.detail_pending() && !renderer.pictures.decoding(),
            "capture must await the requested painting, detail tiles and pictures"
        );
    }

    /// Keys the window's host answers (undo, redo, history, the palette)
    /// or that need the pointer: the canvas is not where they land.
    fn elsewhere(keys: &str) -> bool {
        matches!(keys, "Ctrl+Z" | "Ctrl+Shift+Z" | "Ctrl+Shift+H" | "Ctrl+K")
            || keys.contains("drag")
            || keys.contains("scroll")
    }

    /// The key and modifiers `keys` names, as the canvas receives them.
    fn parse(keys: &str) -> Vec<(Key, ModifiersState)> {
        let mut modifiers = ModifiersState::empty();
        let parts: Vec<&str> = if keys == "+" {
            vec!["+"]
        } else {
            keys.split('+').collect()
        };
        let (last, held) = parts.split_last().unwrap();
        for part in held {
            modifiers |= match *part {
                "Ctrl" => ModifiersState::CONTROL,
                "Shift" => ModifiersState::SHIFT,
                "Alt" => ModifiersState::ALT,
                other => panic!("unknown modifier {other}"),
            };
        }
        let character = |text: &str| Key::Character(text.into());
        let shifted = |text: &str| -> String {
            if !modifiers.shift_key() {
                return text.to_owned();
            }
            match text {
                "1" => "!".into(),
                "0" => ")".into(),
                "2" => "@".into(),
                other => other.to_uppercase(),
            }
        };
        match *last {
            "Arrows" => [
                NamedKey::ArrowLeft,
                NamedKey::ArrowRight,
                NamedKey::ArrowUp,
                NamedKey::ArrowDown,
            ]
            .into_iter()
            .map(|named| (Key::Named(named), modifiers))
            .collect(),
            "Delete" => vec![(Key::Named(NamedKey::Delete), modifiers)],
            "Enter" => vec![(Key::Named(NamedKey::Enter), modifiers)],
            "Escape" => vec![(Key::Named(NamedKey::Escape), modifiers)],
            other => vec![(character(&shifted(&other.to_lowercase())), modifiers)],
        }
    }

    #[test]
    fn every_shortcut_the_overview_lists_does_something() {
        let mut doc = ui::tilo_resource::Doc::new();
        for action in crate::sample::shapes() {
            ui::tilo_actions::Action::apply(&action, &mut doc).unwrap();
        }
        let art = crate::doc::read(&doc);
        let first = art.marks.first().unwrap().id.clone();
        let mut listed: Vec<String> = GROUPS
            .iter()
            .flat_map(|group| {
                group
                    .shortcuts
                    .iter()
                    .map(|shortcut| shortcut.keys.to_owned())
            })
            .collect();
        listed.extend(tools().into_iter().map(|(keys, _)| keys));
        let mut checked = 0;
        for keys in listed.iter().filter(|keys| !elsewhere(keys)) {
            for (key, modifiers) in parse(keys) {
                let mut surface = CanvasSurface::new(Tools::default());
                surface.art = Rc::new(art.clone());
                surface.rect = Rect::new(0.0, 0.0, 800.0, 600.0);
                surface.selection = vec![first.clone()];
                // Grouping takes two marks; ungrouping, a group.
                if keys == "Enter" {
                    // Entering edits a path's points or a text.
                    let editable = art
                        .marks
                        .iter()
                        .find(|mark| {
                            !matches!(mark.shape, Shape::Rect { .. } | Shape::Ellipse { .. })
                        })
                        .unwrap();
                    surface.selection = vec![editable.id.clone()];
                } else if keys == "Ctrl+G" {
                    surface.selection = edit::everything(&art);
                } else if keys == "Ctrl+Shift+G" {
                    let mut grouped = doc.clone();
                    let group = edit::group(&art, &edit::everything(&art)).unwrap();
                    ui::tilo_actions::Action::apply(&group, &mut grouped).unwrap();
                    surface.art = Rc::new(crate::doc::read(&grouped));
                }
                if keys == "Ctrl+V" {
                    ui::clipboard_copy(&edit::copy(&art, &surface.selection).unwrap());
                }
                let text = match &key {
                    Key::Character(text) => Some(text.to_string()),
                    _ => None,
                };
                assert!(
                    surface.key(&key, text.as_deref(), modifiers),
                    "{keys} is listed but the canvas does nothing with it"
                );
                checked += 1;
            }
        }
        assert!(checked > 30, "only {checked} keys were checked");
    }
}
