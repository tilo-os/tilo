//! The canvas as the person sees it: the artwork in a zoomable viewport,
//! drawn on with the pointer. Every finished gesture is one of the
//! artwork's actions; the surface itself never writes the document.
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
use ui::vello::kurbo::{Affine, Point, Rect, Stroke};
use ui::vello::peniko::{Color, Fill};
use ui::widget::{
    AppModel, EventCtx, ModelPath, ResourceOperation, ResourceOperationStart, SelectionHandle,
    SelectionHandles, Widget,
};
use ui::winit::event::{ElementState, MouseButton, WindowEvent};
use ui::winit::keyboard::{Key, NamedKey};
use ui::{LocalState, PresenceCtx, TextRenderer, Theme};

use crate::actions::{CanvasAction, Draw, EditMarks, NewMark, NewStroke, Paint, RemoveMarks};
use crate::model::{Artwork, Shape};
use crate::render::{Options, Renderer};
use crate::resource::{ArtworkResource, VIEW_TYPE, ViewOp};
use crate::view_ops;

/// What the pointer does on the canvas.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tool {
    #[default]
    Select,
    Brush,
    Rect,
    Ellipse,
}

impl Tool {
    pub const ALL: [Self; 4] = [Self::Select, Self::Brush, Self::Rect, Self::Ellipse];

    /// The Fluent id of the tool's name.
    pub fn label(self) -> &'static str {
        match self {
            Self::Select => "canvas-tool-select",
            Self::Brush => "canvas-tool-brush",
            Self::Rect => "canvas-tool-rect",
            Self::Ellipse => "canvas-tool-ellipse",
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
            colour: LocalState::new(Color::from_rgba8(0x2b, 0x3f, 0x8f, 255)),
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
        self.colour.with(|colour| {
            let rgba = colour.to_rgba8();
            format!("#{:02x}{:02x}{:02x}", rgba.r, rgba.g, rgba.b)
        })
    }
}

enum Drag {
    Pan,
    Move {
        start: Point,
        now: Point,
        ids: Vec<String>,
    },
    /// A grip of the selection frame: a uniform resize about the opposite
    /// corner, or the rotation handle.
    Handle {
        handle: SelectionHandle,
        frame: SelectionHandles,
        start: Point,
        now: Point,
        ids: Vec<String>,
    },
    Stroke {
        points: Vec<[f64; 3]>,
    },
    Shape {
        start: Point,
        now: Point,
    },
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
    drag: Option<Drag>,
    selection: Vec<String>,
    cursor: Point,
    focused: bool,
    space: bool,
    shift: bool,
    results: HashMap<tilo_actions::InvocationId, InvocationUpdate>,
}

impl CanvasSurface {
    pub fn new(tools: Tools) -> Self {
        Self {
            rect: Rect::ZERO,
            art: Rc::new(Artwork::default()),
            dispatcher: None,
            read_only: false,
            viewport: ui::widget::ViewportState::new()
                .with_zoom_bounds(0.05, 32.0)
                .with_padding(24.0),
            fitted_to: None,
            moved_by_hand: false,
            renderer: Renderer::default(),
            look_text: None,
            tools,
            drag: None,
            selection: Vec::new(),
            cursor: Point::ZERO,
            focused: false,
            space: false,
            shift: false,
            results: HashMap::new(),
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

    fn content_point(&self, screen: Point) -> Point {
        self.viewport.screen_to_content(screen)
    }

    /// The frame around the selected marks.
    fn frame(&self) -> Option<SelectionHandles> {
        self.selection
            .iter()
            .filter_map(|id| self.art.mark(id).map(crate::model::Mark::bounds))
            .reduce(|a, b| a.union(b))
            .map(SelectionHandles::new)
    }

    fn layer(&self) -> Option<String> {
        let chosen = self.tools.layer.with(Clone::clone);
        if self.art.layer(&chosen).is_some() {
            Some(chosen)
        } else {
            None
        }
    }

    fn press(&mut self, screen: Point) {
        let at = self.content_point(screen);
        if self.space || self.read_only && self.tools.tool.with(|tool| *tool != Tool::Select) {
            self.viewport.begin_pan(screen);
            self.drag = Some(Drag::Pan);
            return;
        }
        let zoom = self.viewport.zoom();
        if self.tools.tool.with(|tool| *tool == Tool::Select)
            && let Some(frame) = self.frame()
            && let Some(handle) = frame.handle_at(at, zoom)
        {
            self.drag = Some(Drag::Handle {
                handle,
                frame,
                start: at,
                now: at,
                ids: self.selection.clone(),
            });
            return;
        }
        self.drag = match self.tools.tool.with(|tool| *tool) {
            Tool::Select => {
                if let Some(mark) = self.art.mark_at(at) {
                    if !self.selection.contains(&mark.id) {
                        self.selection = vec![mark.id.clone()];
                    }
                    Some(Drag::Move {
                        start: at,
                        now: at,
                        ids: self.selection.clone(),
                    })
                } else {
                    self.selection.clear();
                    self.viewport.begin_pan(screen);
                    Some(Drag::Pan)
                }
            }
            Tool::Brush => Some(Drag::Stroke {
                points: vec![[at.x, at.y, 0.6]],
            }),
            Tool::Rect | Tool::Ellipse => Some(Drag::Shape { start: at, now: at }),
        };
    }

    fn drag_to(&mut self, screen: Point) {
        let at = self.content_point(screen);
        let zoom = self.viewport.zoom().max(1e-6);
        match &mut self.drag {
            Some(Drag::Pan) => {
                self.moved_by_hand |= self.viewport.update_pan(screen);
            }
            Some(Drag::Move { now, .. } | Drag::Shape { now, .. } | Drag::Handle { now, .. }) => {
                *now = at;
            }
            Some(Drag::Stroke { points }) => {
                let far_enough = points
                    .last()
                    .is_none_or(|[x, y, _]| (at.x - x).hypot(at.y - y) * zoom >= 3.0);
                if far_enough && points.len() < crate::model::MAX_POINTS {
                    points.push([at.x, at.y, 1.0]);
                }
            }
            None => {}
        }
    }

    fn release(&mut self) {
        let Some(drag) = self.drag.take() else {
            return;
        };
        let layer = self.layer();
        match drag {
            Drag::Pan => {
                self.viewport.end_pan();
            }
            Drag::Move { start, now, ids } => {
                let (dx, dy) = (now.x - start.x, now.y - start.y);
                if dx.hypot(dy) > 0.5 {
                    self.dispatch(&CanvasAction::EditMarks(EditMarks {
                        ids,
                        move_by: Some([dx, dy]),
                        ..EditMarks::default()
                    }));
                }
            }
            Drag::Handle {
                handle,
                frame,
                start,
                now,
                ids,
            } => self.finish_handle(handle, frame, (start, now), ids),
            Drag::Stroke { mut points } => {
                // The pen lifts: ease the last point off as a brush does.
                if let Some(last) = points.last_mut() {
                    last[2] = 0.4;
                }
                self.dispatch(&CanvasAction::Paint(Paint {
                    layer,
                    strokes: vec![NewStroke {
                        id: None,
                        name: None,
                        points: points.into_iter().map(|point| point.to_vec()).collect(),
                        size: self.tools.brush_size(),
                        colour: self.tools.hex(),
                        opacity: None,
                        wet: Some(true),
                        params: None,
                    }],
                }));
            }
            Drag::Shape { start, now } => {
                let rect = Rect::from_points(start, now);
                if rect.width() < 2.0 || rect.height() < 2.0 {
                    return;
                }
                let shape = if self.tools.tool.with(|tool| *tool == Tool::Ellipse) {
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
                };
                self.dispatch(&CanvasAction::Draw(Draw {
                    layer,
                    marks: vec![NewMark {
                        id: None,
                        name: None,
                        shape,
                        fill: Some(self.tools.hex()),
                        gradient: None,
                        stroke: None,
                        stroke_width: None,
                        opacity: None,
                        brush: None,
                        angle: None,
                        wet: None,
                        params: None,
                    }],
                }));
            }
        }
    }

    /// A grip of the selection frame let go: one resize or rotation.
    fn finish_handle(
        &self,
        handle: SelectionHandle,
        frame: SelectionHandles,
        (start, now): (Point, Point),
        ids: Vec<String>,
    ) {
        if start.distance(now) < 0.5 {
            return;
        }
        let edit = if handle == SelectionHandle::Rotate {
            EditMarks {
                ids,
                rotate: Some(frame.rotation_for(start, now, self.shift)),
                ..EditMarks::default()
            }
        } else {
            // Scaling about each mark's centre moves the anchor;
            // move it back so the opposite corner stays put.
            let (scale, anchor) = frame.scale_for(handle, start, now);
            let centre = frame.bounds.center();
            let shift = (anchor - centre) * (1.0 - scale);
            EditMarks {
                ids,
                scale: Some(scale),
                move_by: Some([shift.x, shift.y]),
                ..EditMarks::default()
            }
        };
        self.dispatch(&CanvasAction::EditMarks(edit));
    }

    fn key(&mut self, key: &Key, modifiers: ui::winit::keyboard::ModifiersState) -> bool {
        if self.viewport.on_key(key, &modifiers) {
            self.moved_by_hand = true;
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
                self.dispatch(&CanvasAction::EditMarks(EditMarks {
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
            Key::Named(NamedKey::Escape) if !self.selection.is_empty() => {
                self.selection.clear();
                true
            }
            _ => false,
        }
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

    fn answer(&mut self, op: &ViewOp, doc: &mut AutoCommit) -> Result<ActionOutput, String> {
        let art = crate::doc::read(doc);
        let revision = Self::revision(doc);
        let picture =
            |result: Result<(view_ops::Picture, Value), String>| -> Result<ActionOutput, String> {
                let (picture, mut info) = result?;
                let hash = tilo_content::ContentStore::for_app()
                    .put(&picture.png)
                    .map_err(|error| format!("the picture could not be stored: {error}"))?;
                let address = tilo_actions::content_resource(&hash)
                    .ok_or_else(|| "the picture has no content address".to_owned())?;
                if let Value::Object(fields) = &mut info {
                    fields.insert("revision".into(), revision.clone().into());
                    fields.insert("content".into(), address.clone().into());
                }
                Ok(ActionOutput {
                    value: Some(info),
                    resources: vec![address],
                })
            };
        let text = self.look_text.get_or_insert_with(TextRenderer::new);
        match op {
            ViewOp::Look(look) => picture(view_ops::look(&mut self.renderer, text, &art, look)),
            ViewOp::Export(export) => {
                picture(view_ops::export(&mut self.renderer, text, &art, export))
            }
            ViewOp::Inspect(inspect) => {
                let mut value = view_ops::inspect(&art, inspect)?;
                if let Value::Object(fields) = &mut value {
                    fields.insert("revision".into(), revision.into());
                    fields.insert("selected".into(), json!(self.selection));
                }
                Ok(ActionOutput {
                    value: Some(value),
                    resources: Vec::new(),
                })
            }
            ViewOp::History(view) => Ok(ActionOutput {
                value: Some(view_ops::history(doc, view)),
                resources: Vec::new(),
            }),
            ViewOp::Media => Ok(ActionOutput {
                value: Some(view_ops::media(&art)),
                resources: Vec::new(),
            }),
        }
    }

    fn paint_overlay(&self, scene: &mut Scene, to_screen: Affine, theme: &Theme) {
        let accent = theme.accent_primary;
        let zoom = self.viewport.zoom().max(1e-6);
        let hair = Stroke::new(1.5 / zoom);
        if let Some(frame) = self.frame() {
            let preview = match &self.drag {
                Some(Drag::Move { start, now, .. }) => Affine::translate(*now - *start),
                Some(Drag::Handle {
                    handle,
                    frame,
                    start,
                    now,
                    ..
                }) => {
                    if *handle == SelectionHandle::Rotate {
                        let degrees = frame.rotation_for(*start, *now, self.shift);
                        Affine::rotate_about(degrees.to_radians(), frame.bounds.center())
                    } else {
                        let (scale, anchor) = frame.scale_for(*handle, *start, *now);
                        Affine::scale_about(scale, anchor)
                    }
                }
                _ => Affine::IDENTITY,
            };
            frame.draw(scene, to_screen * preview, zoom, theme);
        }
        match &self.drag {
            Some(Drag::Shape { start, now }) => {
                let rect = Rect::from_points(*start, *now);
                let ghost = self.tools.colour.with(|colour| colour.multiply_alpha(0.6));
                if self.tools.tool.with(|tool| *tool == Tool::Ellipse) {
                    let ellipse = ui::vello::kurbo::Ellipse::from_rect(rect);
                    scene.fill(Fill::NonZero, to_screen, ghost, None, &ellipse);
                } else {
                    scene.fill(Fill::NonZero, to_screen, ghost, None, &rect);
                }
                scene.stroke(&hair, to_screen, accent, None, &rect);
            }
            Some(Drag::Stroke { points }) => {
                let colour = self.tools.colour.with(|colour| *colour);
                let samples: Vec<tilo_wet_media::StrokePoint> = points
                    .iter()
                    .map(|[x, y, pressure]| {
                        #[allow(
                            clippy::cast_possible_truncation,
                            reason = "artwork coordinates are bounded"
                        )]
                        let point = tilo_wet_media::StrokePoint::new(
                            *x as f32,
                            *y as f32,
                            *pressure as f32,
                        );
                        point
                    })
                    .collect();
                #[allow(clippy::cast_possible_truncation, reason = "brush sizes are bounded")]
                let size = self.tools.brush_size() as f32;
                let outline = tilo_wet_media::brush::ink_outline(
                    &tilo_wet_media::brush::smooth(&samples, 0.75),
                    size,
                );
                let mut path = ui::vello::kurbo::BezPath::new();
                for (index, (x, y)) in outline.iter().enumerate() {
                    let point = Point::new(f64::from(*x), f64::from(*y));
                    if index == 0 {
                        path.move_to(point);
                    } else {
                        path.line_to(point);
                    }
                }
                path.close_path();
                scene.fill(
                    Fill::NonZero,
                    to_screen,
                    colour.multiply_alpha(0.8),
                    None,
                    &path,
                );
            }
            _ => {}
        }
    }
}

impl Widget for CanvasSurface {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        use ui::widget::TaffyTreeExt as _;
        let node = taffy.leaf_node(Style {
            size: taffy::Size {
                width: taffy::Dimension::auto(),
                height: taffy::Dimension::percent(1.0),
            },
            flex_basis: taffy::Dimension::length(0.0),
            flex_grow: 1.0,
            flex_shrink: 1.0,
            min_size: taffy::Size {
                width: taffy::Dimension::length(120.0),
                height: taffy::Dimension::length(120.0),
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
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
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
        let zoom = self.viewport.zoom().max(1e-6);
        let shadow = theme.border_standard.multiply_alpha(0.5);
        scene.fill(
            Fill::NonZero,
            to_screen,
            shadow,
            None,
            &(board + ui::vello::kurbo::Vec2::new(3.0 / zoom, 4.0 / zoom)),
        );
        self.renderer.draw(
            &self.art,
            scene,
            to_screen,
            text_renderer,
            &Options::default(),
        );
        scene.stroke(
            &Stroke::new(1.0 / zoom),
            to_screen,
            theme.border_standard,
            None,
            &board,
        );
        self.paint_overlay(scene, to_screen, theme);
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
                self.shift = modifiers.state().shift_key();
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
                    *ctx.handled = true;
                }
            }
            WindowEvent::KeyboardInput { event, .. } if self.focused => {
                if event.logical_key == Key::Named(NamedKey::Space) {
                    self.space = event.state == ElementState::Pressed;
                    *ctx.handled = true;
                } else if event.state == ElementState::Pressed
                    && self.key(&event.logical_key, ctx.modifiers.state())
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
                    "{} by {} units, {layers} layers, {marks} marks; inspect lists them and look shows them",
                    self.art.width, self.art.height
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

    fn cursor_shape(&self, point: Point) -> Option<ui::winit::window::CursorIcon> {
        if !self.rect.contains(point) {
            return None;
        }
        Some(match (self.space, self.tools.tool.with(|tool| *tool)) {
            (true, _) => ui::winit::window::CursorIcon::Grab,
            (false, Tool::Select) => ui::winit::window::CursorIcon::Default,
            (false, _) => ui::winit::window::CursorIcon::Crosshair,
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
        self.selection.retain(|id| state.mark(id).is_some());
        self.art = Rc::clone(state);
    }

    fn attach(&mut self, cx: SurfaceCx<ArtworkResource>) {
        self.read_only = cx.read_only();
        self.dispatcher = Some(cx.dispatcher());
    }

    fn read_only_changed(&mut self, read_only: bool) {
        self.read_only = read_only;
    }

    /// The landscape screenshot shows a selection with its handles.
    fn stage(&mut self, scenario: Option<&str>) {
        if scenario == Some("landscape") {
            self.selection = vec!["sun".into()];
        }
    }
}
