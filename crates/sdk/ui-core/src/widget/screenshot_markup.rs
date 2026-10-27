//! Exact-pixel screenshot annotation surface shared by applications and shell.
//!
//! The widget keeps strokes in source-image coordinates. Presentation may be
//! fitted to any window, but save/copy always rasterise back onto the original
//! pixels; a 7,000px-tall full-app capture therefore never becomes a scaled
//! screen grab merely because it was edited on a laptop display.

#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::indexing_slicing,
    clippy::iter_with_drain,
    clippy::too_many_lines,
    reason = "raster coordinates and fixed-size color/point buffers are locally bounded"
)]

use std::{collections::HashMap, sync::Arc};

use crate::time::{Duration, Instant};

use taffy::{NodeId, Position, Size, Style, TaffyTree};
use vello::{
    Scene,
    kurbo::{Affine, BezPath, Point, Rect, RoundedRect, Stroke, Vec2},
    peniko::{BlendMode, Color, Fill},
};
use winit::{
    event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent},
    keyboard::{Key, NamedKey},
};

use crate::tokens::TextRole;
use crate::{
    ColorCompat,
    icons::{self, Icon},
    presence::PresenceCtx,
    surface,
    text_render::{TextRenderer, TextWeight},
    theme::Theme,
    widget::{Image, ImageFit, TaffyTreeExt, Widget},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MarkupTool {
    #[default]
    Pen,
    Highlighter,
    Arrow,
    Rectangle,
    Redact,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MarkupStroke {
    pub tool: MarkupTool,
    pub color: [u8; 4],
    pub width_px: f32,
    pub points: Vec<Point>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MarkupExport {
    pub width: u32,
    pub height: u32,
    pub rgba: Arc<[u8]>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScreenshotMarkupCommand {
    Save(MarkupExport),
    Copy(MarkupExport),
    OpenExternal,
    Cancel,
}

/// A modal editor, intentionally implemented in `ui` rather than shell so an
/// image app can host the identical canvas and controls.
pub struct ScreenshotMarkup {
    image: Image,
    original: Arc<[u8]>,
    width: u32,
    height: u32,
    rect: Rect,
    viewport: Rect,
    fit_canvas: Rect,
    canvas: Rect,
    toolbar: Rect,
    cursor: Point,
    tool: MarkupTool,
    tool_picker_open: bool,
    /// The colour palette disclosure, used only when the toolbar is too
    /// narrow to show every swatch inline.
    color_picker_open: bool,
    color: [u8; 4],
    strokes: Vec<MarkupStroke>,
    redo: Vec<MarkupStroke>,
    drawing: Option<MarkupStroke>,
    zoom: f64,
    pan: Vec2,
    panning: Option<Point>,
    pending: Vec<ScreenshotMarkupCommand>,
    opened_at: Instant,
    spawn_from: Option<Rect>,
    entrance_progress_override: Option<f64>,
    /// The toolbar control the keyboard is on (Tab moves it, Space and
    /// Enter press it); `None` while the keyboard is on the editor itself.
    keyboard: Option<&'static str>,
    focused: bool,
}

/// The editor opening out of the captured shot: one thing becoming another
/// in place (`motion::Zoom`).
fn markup_entrance(elapsed: Duration) -> f64 {
    crate::motion::Zoom::SPEC.progress_after(
        crate::motion::Direction::Enter,
        crate::tokens::EffectiveMotion::default(),
        crate::tokens::MotionSource::Pointer,
        elapsed,
    )
}

fn markup_entrance_time() -> Duration {
    crate::motion::Zoom::SPEC.settle_time(
        crate::motion::Direction::Enter,
        crate::tokens::EffectiveMotion::default(),
        crate::tokens::MotionSource::Pointer,
    )
}
/// Gutter between the surface edge and the canvas, and the header's left
/// and right insets — one gutter for the whole editor. Equals `spacing.s6`.
const CANVAS_INSET: f64 = 24.0;
/// The header band above the canvas; the title, caption and zoom hint are
/// centred on its middle.
const HEADER_BAND_H: f64 = 54.0;
/// Gap between the title and its caption. Equals `spacing.s2`.
const HEADER_GAP: f64 = 8.0;
/// Minimum pointer target for a toolbar control (HIG §9.6).
const MIN_TARGET: f64 = 32.0;
/// Gap between neighbouring toolbar controls. Equals `spacing.s1`.
const CONTROL_GAP: f64 = 4.0;
/// Inset from the toolbar's rounded ends to its first and last control.
/// Equals `spacing.s2`.
const TOOLBAR_INSET: f64 = 8.0;

/// The toolbar's geometry, computed once per query by
/// [`ScreenshotMarkup::toolbar_layout`] and read by paint, hit-testing and
/// the accessibility rects alike.
struct ToolbarLayout {
    /// `(id, rect)` for every visible control; the rect is both the painted
    /// footprint and the pointer target.
    controls: Vec<(&'static str, Rect)>,
    /// Where the "Opaque" badge stands while the redact tool is active;
    /// `Rect::ZERO` when the toolbar has no room for it.
    redact_badge: Rect,
    tool_picker: Rect,
    /// The colour palette pill, anchored above the `colors` button — only
    /// present when the swatches did not fit inline and the picker is open.
    color_picker: Option<Rect>,
}

const SWATCHES: [[u8; 4]; 5] = [
    [244, 84, 92, 255],
    [255, 190, 70, 255],
    [82, 160, 255, 255],
    [111, 221, 166, 255],
    [245, 245, 247, 255],
];
/// Control ids for [`SWATCHES`], index for index — the same ids whether a
/// swatch sits inline in the toolbar or in the colour palette pill.
const SWATCH_IDS: [&str; 5] = [
    "color-red",
    "color-yellow",
    "color-blue",
    "color-green",
    "color-white",
];

impl ScreenshotMarkup {
    pub fn new(rgba: Arc<[u8]>, width: u32, height: u32) -> Self {
        let image = Image::from_rgba(rgba.clone(), width, height)
            .with_fit(ImageFit::Fill)
            .with_alt("Screenshot being marked up");
        Self {
            image,
            original: rgba,
            width,
            height,
            rect: Rect::ZERO,
            viewport: Rect::ZERO,
            fit_canvas: Rect::ZERO,
            canvas: Rect::ZERO,
            toolbar: Rect::ZERO,
            cursor: Point::ZERO,
            tool: MarkupTool::Pen,
            tool_picker_open: false,
            color_picker_open: false,
            color: SWATCHES[0],
            strokes: Vec::new(),
            redo: Vec::new(),
            drawing: None,
            zoom: 1.0,
            pan: Vec2::ZERO,
            panning: None,
            pending: Vec::new(),
            opened_at: crate::time::now(),
            spawn_from: None,
            entrance_progress_override: None,
            keyboard: None,
            focused: false,
        }
    }

    #[must_use]
    pub fn with_spawn_from(mut self, rect: Option<Rect>) -> Self {
        self.spawn_from = rect.filter(|rect| rect.area() > 1.0);
        self
    }

    fn entrance_motion(&self) -> f64 {
        if let Some(progress) = self.entrance_progress_override {
            return progress;
        }
        markup_entrance(crate::time::now().saturating_duration_since(self.opened_at))
    }

    /// Freeze the entrance for deterministic screenshot review. Production
    /// advances from `Instant`; this only replaces the clock, not geometry.
    #[doc(hidden)]
    pub fn set_entrance_transition_progress(&mut self, permille: u16) {
        self.entrance_progress_override = Some(f64::from(permille.min(1_000)) / 1_000.0);
    }

    fn presentation_canvas(&self, p: f64) -> Rect {
        let start = self
            .spawn_from
            .unwrap_or_else(|| Rect::from_center_size(self.canvas.center(), (48.0, 28.0)));
        Rect::new(
            start.x0 + (self.canvas.x0 - start.x0) * p,
            start.y0 + (self.canvas.y0 - start.y0) * p,
            start.x1 + (self.canvas.x1 - start.x1) * p,
            start.y1 + (self.canvas.y1 - start.y1) * p,
        )
    }

    pub fn strokes(&self) -> &[MarkupStroke] {
        &self.strokes
    }

    pub fn take_commands(&mut self) -> Vec<ScreenshotMarkupCommand> {
        std::mem::take(&mut self.pending)
    }

    pub fn flattened(&self) -> MarkupExport {
        let mut rgba = self.original.to_vec();
        for stroke in &self.strokes {
            raster_stroke(&mut rgba, self.width, self.height, stroke);
        }
        if let Some(stroke) = &self.drawing {
            raster_stroke(&mut rgba, self.width, self.height, stroke);
        }
        MarkupExport {
            width: self.width,
            height: self.height,
            rgba: Arc::from(rgba),
        }
    }

    fn layout_parts(&mut self) {
        let available = Rect::new(
            self.rect.x0 + CANVAS_INSET,
            self.rect.y0 + HEADER_BAND_H,
            self.rect.x1 - CANVAS_INSET,
            self.rect.y1 - 96.0,
        );
        let source_width = f64::from(self.width.max(1));
        let source_height = f64::from(self.height.max(1));
        // Fit large captures into the workspace, but keep small screenshots at
        // one screen pixel per source pixel. An editor should not quietly blur
        // a capture just because there happens to be room around it.
        let fit_scale = (available.width() / source_width)
            .min(available.height() / source_height)
            .min(1.0);
        let size = (source_width * fit_scale, source_height * fit_scale);
        self.fit_canvas = Rect::from_center_size(available.center(), size);
        self.viewport = self.fit_canvas;
        self.update_canvas();
        let toolbar_width = self.rect.width().min(620.0) - 32.0;
        self.toolbar = Rect::from_center_size(
            Point::new(self.rect.center().x, self.rect.y1 - 48.0),
            (toolbar_width.max(420.0), 52.0),
        );
    }

    fn update_canvas(&mut self) {
        let center = self.fit_canvas.center() + self.pan;
        self.canvas = Rect::from_center_size(
            center,
            (
                self.fit_canvas.width() * self.zoom,
                self.fit_canvas.height() * self.zoom,
            ),
        );
    }

    fn clamp_pan(&mut self) {
        let max_x = ((self.fit_canvas.width() * self.zoom - self.viewport.width()) / 2.0).max(0.0);
        let max_y =
            ((self.fit_canvas.height() * self.zoom - self.viewport.height()) / 2.0).max(0.0);
        self.pan.x = self.pan.x.clamp(-max_x, max_x);
        self.pan.y = self.pan.y.clamp(-max_y, max_y);
        self.update_canvas();
    }

    fn set_zoom_around(&mut self, zoom: f64, anchor: Point) {
        let zoom = zoom.clamp(1.0, 8.0);
        if (zoom - self.zoom).abs() < 0.001 {
            return;
        }
        let image_point = self.image_point(anchor);
        self.zoom = zoom;
        self.update_canvas();
        let sx = self.canvas.width() / f64::from(self.width.max(1));
        let sy = self.canvas.height() / f64::from(self.height.max(1));
        let mapped = Point::new(
            self.canvas.x0 + image_point.x * sx,
            self.canvas.y0 + image_point.y * sy,
        );
        self.pan += anchor - mapped;
        self.clamp_pan();
    }

    fn reset_view(&mut self) {
        self.zoom = 1.0;
        self.pan = Vec2::ZERO;
        self.update_canvas();
    }

    fn display_scale(&self) -> f64 {
        self.canvas.width() / f64::from(self.width.max(1))
    }

    fn move_active_gesture(&mut self, point: Point) -> bool {
        self.cursor = point;
        if let Some(previous) = self.panning {
            self.panning = Some(self.cursor);
            self.pan += self.cursor - previous;
            self.clamp_pan();
            true
        } else if let Some(mut stroke) = self.drawing.take() {
            stroke.points.push(self.image_point(self.cursor));
            self.drawing = Some(stroke);
            true
        } else {
            false
        }
    }

    /// One geometry description for the toolbar. Every control rect, the
    /// redact badge and the tool picker come out of the same left-to-right
    /// placement, so paint, hit-testing and the accessibility rects cannot
    /// disagree, and nothing re-derives a position from a literal sum of
    /// its neighbours' widths.
    ///
    /// The action cluster is placed first, from the right end; the colour
    /// swatches then take only the room left between the tool group and
    /// the actions. On a surface too narrow for all five, swatches drop
    /// from the right rather than sliding under the action buttons — a
    /// control half under another control is worse than one not offered.
    fn toolbar_layout(&self) -> ToolbarLayout {
        let y = self.toolbar.center().y;
        let control = |cx: f64, w: f64| Rect::from_center_size(Point::new(cx, y), (w, 36.0));
        let mut controls: Vec<(&'static str, Rect)> = Vec::new();

        let save = Rect::new(
            self.toolbar.x1 - 82.0,
            y - 18.0,
            self.toolbar.x1 - TOOLBAR_INSET,
            y + 18.0,
        );
        let copy = Rect::new(save.x0 - 40.0, y - 18.0, save.x0 - CONTROL_GAP, y + 18.0);
        let open = Rect::new(copy.x0 - 40.0, y - 18.0, copy.x0 - CONTROL_GAP, y + 18.0);

        let mut x = self.toolbar.x0 + TOOLBAR_INSET;
        let mut tool_center_x = x;
        for (id, width) in [
            ("cancel", 36.0),
            ("undo", 36.0),
            ("redo", 36.0),
            ("tool", 40.0),
            ("fit", 36.0),
        ] {
            let r = control(x + width / 2.0, width);
            if id == "tool" {
                tool_center_x = r.center().x;
            }
            controls.push((id, r));
            x += width + CONTROL_GAP;
        }
        let group_end = x;
        // Everything between the tool group and the action cluster has to
        // fit before `limit`; nothing may run under the actions.
        let limit = open.x0 - CONTROL_GAP;

        // A 216 px palette pill with five 36 px choices on a 42 px stride,
        // anchored above `anchor` and kept inside the surface.
        let palette_above = |anchor: Rect| {
            let mut pill = Rect::from_center_size(
                Point::new(anchor.center().x + 84.0, self.toolbar.y0 - 30.0),
                (216.0, 44.0),
            );
            let overshoot = (pill.x1 - (self.rect.x1 - TOOLBAR_INSET)).max(0.0);
            let undershoot = ((self.rect.x0 + TOOLBAR_INSET) - pill.x0).max(0.0);
            pill = pill + Vec2::new(undershoot - overshoot, 0.0);
            pill
        };
        let choice_in = |pill: Rect, index: usize| {
            Rect::from_center_size(
                Point::new(pill.x0 + 24.0 + index as f64 * 42.0, pill.center().y),
                (36.0, 36.0),
            )
        };

        let mut color_picker = None;
        if self.tool != MarkupTool::Redact {
            let swatch_run = MIN_TARGET * SWATCHES.len() as f64;
            if x + swatch_run <= limit {
                // Room for every swatch inline, each a full minimum pointer
                // target; the dot itself stays small, the target does not.
                for index in 0..SWATCHES.len() {
                    controls.push((SWATCH_IDS[index], control(x + MIN_TARGET / 2.0, MIN_TARGET)));
                    x += MIN_TARGET;
                }
            } else {
                // Too narrow for five targets side by side: one `colors`
                // button showing the current colour opens the palette above
                // the toolbar, the way the tool button does — every colour
                // stays reachable by pointer and by its `markup.color-*`
                // target instead of vanishing with the width.
                let r = control(x + 20.0, 40.0);
                if r.x1 <= limit {
                    controls.push(("colors", r));
                    if self.color_picker_open {
                        let pill = palette_above(r);
                        for index in 0..SWATCHES.len() {
                            controls.push((SWATCH_IDS[index], choice_in(pill, index)));
                        }
                        color_picker = Some(pill);
                    }
                }
            }
        }
        controls.extend([("open", open), ("copy", copy), ("save", save)]);

        // The badge stands where the swatches would, immediately after the
        // tool group, and shrinks to the room left before the actions rather
        // than running under them; below a legible width it is not shown.
        let badge_w = (limit - group_end).min(96.0);
        let redact_badge = if badge_w >= 44.0 {
            Rect::new(group_end, y - 16.0, group_end + badge_w, y + 16.0)
        } else {
            Rect::ZERO
        };
        // Keep the first choice directly above the invoking tool button;
        // each additional 42px choice grows the pill to the right.
        let tool_picker = Rect::from_center_size(
            Point::new(tool_center_x + 84.0, self.toolbar.y0 - 30.0),
            (216.0, 44.0),
        );
        if self.tool_picker_open {
            for (index, id) in [
                "tool-pen",
                "tool-highlighter",
                "tool-arrow",
                "tool-rectangle",
                "tool-redact",
            ]
            .into_iter()
            .enumerate()
            {
                controls.push((id, choice_in(tool_picker, index)));
            }
        }
        ToolbarLayout {
            controls,
            redact_badge,
            tool_picker,
            color_picker,
        }
    }

    fn color_picker_rect(&self) -> Option<Rect> {
        self.toolbar_layout().color_picker
    }

    fn controls(&self) -> Vec<(&'static str, Rect)> {
        self.toolbar_layout().controls
    }

    fn redact_badge_rect(&self) -> Rect {
        self.toolbar_layout().redact_badge
    }

    fn tool_picker_rect(&self) -> Rect {
        self.toolbar_layout().tool_picker
    }

    fn image_point(&self, p: Point) -> Point {
        Point::new(
            ((p.x - self.canvas.x0) / self.canvas.width() * f64::from(self.width))
                .clamp(0.0, f64::from(self.width.saturating_sub(1))),
            ((p.y - self.canvas.y0) / self.canvas.height() * f64::from(self.height))
                .clamp(0.0, f64::from(self.height.saturating_sub(1))),
        )
    }

    fn activate(&mut self, id: &str) {
        match id {
            "cancel" => self.pending.push(ScreenshotMarkupCommand::Cancel),
            "undo" => {
                if let Some(stroke) = self.strokes.pop() {
                    self.redo.push(stroke);
                }
            }
            "redo" => {
                if let Some(stroke) = self.redo.pop() {
                    self.strokes.push(stroke);
                }
            }
            "tool" => {
                self.tool_picker_open = !self.tool_picker_open;
                self.color_picker_open = false;
            }
            "colors" => {
                self.color_picker_open = !self.color_picker_open;
                self.tool_picker_open = false;
            }
            "tool-pen" => {
                self.tool = MarkupTool::Pen;
                self.tool_picker_open = false;
            }
            "tool-highlighter" => {
                self.tool = MarkupTool::Highlighter;
                self.tool_picker_open = false;
            }
            "tool-arrow" => {
                self.tool = MarkupTool::Arrow;
                self.tool_picker_open = false;
            }
            "tool-rectangle" => {
                self.tool = MarkupTool::Rectangle;
                self.tool_picker_open = false;
            }
            "tool-redact" => {
                self.tool = MarkupTool::Redact;
                self.tool_picker_open = false;
            }
            "fit" => self.reset_view(),
            "open" => self.pending.push(ScreenshotMarkupCommand::OpenExternal),
            "copy" => self
                .pending
                .push(ScreenshotMarkupCommand::Copy(self.flattened())),
            "save" => self
                .pending
                .push(ScreenshotMarkupCommand::Save(self.flattened())),
            id if id.starts_with("color-") => {
                if let Some(index) = SWATCH_IDS.iter().position(|candidate| *candidate == id) {
                    self.color = SWATCHES[index];
                }
                // Picking from the palette is a choice; the pill closes.
                self.color_picker_open = false;
            }
            _ => {}
        }
    }

    /// Every toolbar control in reading order with its catalogue label: the
    /// order Tab walks and the order assistive technology lists them. The
    /// tool picker's choices follow the button that opened them, as the
    /// colour palette's already follow theirs.
    fn semantic_controls(&self) -> Vec<(&'static str, Rect, &'static str)> {
        let controls = self.controls();
        let (choices, rest): (Vec<_>, Vec<_>) = controls
            .into_iter()
            .partition(|(id, _)| id.starts_with("tool-"));
        let mut ordered = Vec::with_capacity(rest.len() + choices.len());
        for control in rest {
            let opens_choices = control.0 == "tool";
            ordered.push(control);
            if opens_choices {
                ordered.extend(choices.iter().copied());
            }
        }
        ordered
            .into_iter()
            .filter_map(|(id, rect)| self.control_label_key(id).map(|key| (id, rect, key)))
            .collect()
    }

    /// The catalogue key naming one toolbar control, shown as its tooltip
    /// and announced by assistive technology.
    fn control_label_key(&self, id: &str) -> Option<&'static str> {
        Some(match id {
            "cancel" => "widget-markup-close",
            "undo" => "widget-markup-undo",
            "redo" => "widget-markup-redo",
            "tool" => match self.tool {
                MarkupTool::Pen => "widget-markup-tools-pen",
                MarkupTool::Highlighter => "widget-markup-tools-highlighter",
                MarkupTool::Arrow => "widget-markup-tools-arrow",
                MarkupTool::Rectangle => "widget-markup-tools-rectangle",
                MarkupTool::Redact => "widget-markup-tools-redact",
            },
            "tool-pen" => "widget-markup-tool-pen",
            "tool-highlighter" => "widget-markup-tool-highlighter",
            "tool-arrow" => "widget-markup-tool-arrow",
            "tool-rectangle" => "widget-markup-tool-rectangle",
            "tool-redact" => "widget-markup-tool-redact",
            "fit" => "widget-markup-fit",
            "colors" => "widget-markup-colors",
            "color-red" => "widget-markup-color-red",
            "color-yellow" => "widget-markup-color-yellow",
            "color-blue" => "widget-markup-color-blue",
            "color-green" => "widget-markup-color-green",
            "color-white" => "widget-markup-color-white",
            "open" => "widget-markup-open",
            "copy" => "widget-markup-copy",
            "save" => "widget-markup-save",
            _ => return None,
        })
    }

    /// The control the keyboard is on, if it is still offered: a picker
    /// choice leaves with its picker, a swatch with a narrowing toolbar.
    fn keyboard_control(&self) -> Option<&'static str> {
        let id = self.keyboard?;
        self.controls()
            .iter()
            .any(|(candidate, _)| *candidate == id)
            .then_some(id)
    }

    /// Press one toolbar control (pointer, keyboard or assistive
    /// technology alike); a keyboard on a choice that closed returns to the
    /// button that opened it.
    fn press(&mut self, id: &str) {
        self.activate(id);
        if let Some(on) = self.keyboard
            && self.keyboard_control().is_none()
        {
            self.keyboard = if on.starts_with("tool-") {
                Some("tool")
            } else if on.starts_with("color-") {
                Some("colors")
            } else {
                None
            };
        }
    }

    /// Close the open picker through its button, so a keyboard on one of
    /// its choices returns to that button rather than to nothing (where
    /// Enter would save and close the editor).
    fn close_picker(&mut self) {
        if self.tool_picker_open {
            self.press("tool");
        } else if self.color_picker_open {
            self.press("colors");
        }
    }

    /// Tab and Shift+Tab: the next or previous toolbar control, wrapping.
    fn move_keyboard(&mut self, back: bool) {
        let ids: Vec<&'static str> = self
            .semantic_controls()
            .into_iter()
            .map(|(id, _, _)| id)
            .collect();
        let Some(last) = ids.len().checked_sub(1) else {
            return;
        };
        let current = self
            .keyboard_control()
            .and_then(|on| ids.iter().position(|id| *id == on));
        let next = match (current, back) {
            (None, false) => 0,
            (None | Some(0), true) => last,
            (Some(index), false) if index >= last => 0,
            (Some(index), false) => index + 1,
            (Some(index), true) => index - 1,
        };
        self.keyboard = ids.get(next).copied();
    }

    fn draw_stroke(&self, scene: &mut Scene, transform: Affine, stroke: &MarkupStroke) {
        if stroke.points.is_empty() {
            return;
        }
        let sx = self.canvas.width() / f64::from(self.width.max(1));
        let sy = self.canvas.height() / f64::from(self.height.max(1));
        let map = |p: Point| Point::new(self.canvas.x0 + p.x * sx, self.canvas.y0 + p.y * sy);
        let color = Color::from_rgba8(
            stroke.color[0],
            stroke.color[1],
            stroke.color[2],
            stroke.color[3],
        );
        if stroke.tool == MarkupTool::Redact && stroke.points.len() > 1 {
            let start = map(stroke.points[0]);
            let end = map(*stroke.points.last().unwrap_or(&stroke.points[0]));
            let redaction = Rect::new(
                start.x.min(end.x),
                start.y.min(end.y),
                start.x.max(end.x),
                start.y.max(end.y),
            );
            scene.fill(Fill::NonZero, transform, color, None, &redaction);
            return;
        }
        let mut path = BezPath::new();
        if stroke.tool == MarkupTool::Arrow && stroke.points.len() > 1 {
            let start = stroke.points[0];
            let end = *stroke.points.last().unwrap_or(&start);
            path.move_to(map(start));
            path.line_to(map(end));
            if let Some((left, right)) = arrow_head(start, end, f64::from(stroke.width_px)) {
                path.move_to(map(left));
                path.line_to(map(end));
                path.line_to(map(right));
            }
        } else if stroke.tool == MarkupTool::Rectangle && stroke.points.len() > 1 {
            let start = stroke.points[0];
            let end = *stroke.points.last().unwrap_or(&start);
            let [top_left, top_right, bottom_right, bottom_left] = rectangle_points(start, end);
            path.move_to(map(top_left));
            path.line_to(map(top_right));
            path.line_to(map(bottom_right));
            path.line_to(map(bottom_left));
            path.close_path();
        } else {
            path.move_to(map(stroke.points[0]));
            for point in stroke.points.iter().skip(1) {
                path.line_to(map(*point));
            }
        }
        if stroke.points.len() == 1 {
            let p = map(stroke.points[0]);
            path.line_to(Point::new(p.x + 0.01, p.y + 0.01));
        }
        scene.stroke(
            &Stroke::new(f64::from(stroke.width_px) * sx.min(sy))
                .with_caps(vello::kurbo::Cap::Round)
                .with_join(vello::kurbo::Join::Round),
            transform,
            color,
            None,
            &path,
        );
    }
}

impl Widget for ScreenshotMarkup {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let style = Style {
            position: Position::Absolute,
            size: Size::from_percent(1.0, 1.0),
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
        self.layout_parts();
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        _: &PresenceCtx,
    ) {
        if self.rect.area() <= 0.0 {
            return;
        }
        let enter = if theme.reduce_motion {
            1.0
        } else {
            self.entrance_motion()
        };
        let settled_canvas = self.canvas;
        let settled_toolbar = self.toolbar;
        self.canvas = self.presentation_canvas(enter);
        self.toolbar = self.toolbar + vello::kurbo::Vec2::new(0.0, 12.0 * (1.0 - enter));
        let canvas_frame = if enter < 0.999 {
            self.canvas
        } else {
            self.viewport
        };
        let c = &theme.tokens.colors;
        scene.fill(
            Fill::NonZero,
            transform,
            c.surfaces.overlay.with_alpha_factor((0.94 * enter) as f32),
            None,
            &self.rect,
        );
        // Header: title, its caption and the zoom hint share one measured
        // centre line. The caption follows the measured title rather than a
        // fixed offset, so a larger type scale cannot run the two together.
        let title = &markup_text("widget-markup-title");
        let title_style = theme.text(TextRole::Body);
        let caption_style = theme.text(TextRole::Caption);
        let (title_w, title_h) = tr.measure_style(
            title,
            None,
            &title_style.clone().with_weight(TextWeight::Semibold),
        );
        let (_, caption_h) = tr.measure_style("Ag", None, &caption_style);
        let header_cy = self.rect.y0 + HEADER_BAND_H / 2.0;
        let title_x = self.rect.x0 + CANVAS_INSET;
        tr.draw_style(
            scene,
            title,
            c.text_primary.with_alpha_factor(enter as f32),
            transform.pre_translate((title_x, header_cy - f64::from(title_h) / 2.0).into()),
            None,
            &title_style.clone().with_weight(TextWeight::Semibold),
        );
        let caption_y = header_cy - f64::from(caption_h) / 2.0;
        let hint = format!(
            "{:.0}% · wheel zoom · middle-drag",
            self.display_scale() * 100.0
        );
        let (hint_w, _) = tr.measure_style(&hint, None, &caption_style);
        let hint_x = self.rect.x1 - CANVAS_INSET - f64::from(hint_w);
        let caption_x = title_x + f64::from(title_w) + HEADER_GAP;
        // Elided to the room left before the hint, never wrapped: a wrap
        // width would fold the caption into a column under the header.
        let caption = tr.elide_to_width_style(
            &format!("{} × {} · original pixels", self.width, self.height),
            (hint_x - HEADER_GAP - caption_x).max(0.0) as f32,
            &caption_style,
        );
        tr.draw_style(
            scene,
            &caption,
            c.text_dim.with_alpha_factor(enter as f32),
            transform.pre_translate((caption_x, caption_y).into()),
            None,
            &caption_style,
        );
        tr.draw_style(
            scene,
            &hint,
            c.text_dim.with_alpha_factor(enter as f32),
            transform.pre_translate((hint_x, caption_y).into()),
            None,
            &caption_style,
        );
        surface::shadow(
            scene,
            transform,
            canvas_frame,
            8.0,
            &theme.tokens.elevation.floating,
        );
        scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            1.0,
            transform,
            &RoundedRect::from_rect(canvas_frame, 8.0),
        );
        self.image.paint_at_on(scene, transform, self.canvas, theme);
        for stroke in &self.strokes {
            self.draw_stroke(scene, transform, stroke);
        }
        if let Some(stroke) = &self.drawing {
            self.draw_stroke(scene, transform, stroke);
        }
        scene.pop_layer();
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            crate::tokens::colors::Colors::hairline_on(c.surfaces.base, 26),
            None,
            &RoundedRect::from_rect(canvas_frame.inflate(-0.5, -0.5), 7.5),
        );

        scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            enter as f32,
            transform,
            &self.rect,
        );
        surface::shadow(
            scene,
            transform,
            self.toolbar,
            26.0,
            &theme.tokens.elevation.floating,
        );
        scene.fill(
            Fill::NonZero,
            transform,
            c.surfaces.raised,
            None,
            &RoundedRect::from_rect(self.toolbar, 26.0),
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            crate::tokens::colors::Colors::hairline_on(c.surfaces.raised, 26),
            None,
            &RoundedRect::from_rect(self.toolbar.inflate(-0.5, -0.5), 25.5),
        );
        if self.tool_picker_open {
            let palette = self.tool_picker_rect();
            surface::shadow(
                scene,
                transform,
                palette,
                22.0,
                &theme.tokens.elevation.floating,
            );
            scene.fill(
                Fill::NonZero,
                transform,
                c.surfaces.raised,
                None,
                &RoundedRect::from_rect(palette, 22.0),
            );
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                crate::tokens::colors::Colors::hairline_on(c.surfaces.raised, 26),
                None,
                &RoundedRect::from_rect(palette.inflate(-0.5, -0.5), 21.5),
            );
        }
        if let Some(palette) = self.color_picker_rect() {
            surface::shadow(
                scene,
                transform,
                palette,
                22.0,
                &theme.tokens.elevation.floating,
            );
            scene.fill(
                Fill::NonZero,
                transform,
                c.surfaces.raised,
                None,
                &RoundedRect::from_rect(palette, 22.0),
            );
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                crate::tokens::colors::Colors::hairline_on(c.surfaces.raised, 26),
                None,
                &RoundedRect::from_rect(palette.inflate(-0.5, -0.5), 21.5),
            );
        }
        let badge = self.redact_badge_rect();
        if self.tool == MarkupTool::Redact && badge.width() > 0.0 {
            scene.fill(
                Fill::NonZero,
                transform,
                c.lift(0.35),
                None,
                &RoundedRect::from_rect(badge, badge.height() / 2.0),
            );
            icons::draw_icon(
                scene,
                transform,
                Rect::from_center_size(Point::new(badge.x0 + 17.0, badge.center().y), (14.0, 14.0)),
                Icon::Shield,
                c.text_dim,
                1.4,
            );
            let (_, opaque_h) = tr.measure_style(
                "Opaque",
                None,
                &theme
                    .text(TextRole::Caption)
                    .with_weight(TextWeight::Medium),
            );
            // Elided to the badge, which may have shrunk to the room left.
            let opaque = tr.elide_to_width_style(
                "Opaque",
                (badge.width() - 29.0 - 8.0).max(0.0) as f32,
                &theme
                    .text(TextRole::Caption)
                    .with_weight(TextWeight::Medium),
            );
            tr.draw_style(
                scene,
                &opaque,
                c.text_dim,
                transform.pre_translate(
                    (
                        badge.x0 + 29.0,
                        badge.center().y - f64::from(opaque_h) / 2.0,
                    )
                        .into(),
                ),
                None,
                &theme
                    .text(TextRole::Caption)
                    .with_weight(TextWeight::Medium),
            );
        }
        for (id, r) in self.controls() {
            let selected = (id == "tool" && self.tool_picker_open)
                || (id == "colors" && self.color_picker_open)
                || (id == "tool-pen" && self.tool == MarkupTool::Pen)
                || (id == "tool-highlighter" && self.tool == MarkupTool::Highlighter)
                || (id == "tool-arrow" && self.tool == MarkupTool::Arrow)
                || (id == "tool-rectangle" && self.tool == MarkupTool::Rectangle)
                || (id == "tool-redact" && self.tool == MarkupTool::Redact);
            let hovered = r.contains(self.cursor);
            if selected || hovered || id == "save" {
                scene.fill(
                    Fill::NonZero,
                    transform,
                    if id == "save" {
                        c.intent.accent
                    } else if selected {
                        c.intent.accent_hover.with_alpha_factor(0.22)
                    } else {
                        c.lift(0.72)
                    },
                    None,
                    &RoundedRect::from_rect(r, r.height() / 2.0),
                );
            }
            if let Some(index) = [
                "color-red",
                "color-yellow",
                "color-blue",
                "color-green",
                "color-white",
            ]
            .iter()
            .position(|candidate| *candidate == id)
            {
                let swatch = vello::kurbo::Circle::new(
                    r.center(),
                    if self.color == SWATCHES[index] {
                        8.0
                    } else {
                        6.0
                    },
                );
                scene.fill(
                    Fill::NonZero,
                    transform,
                    Color::from_rgba8(
                        SWATCHES[index][0],
                        SWATCHES[index][1],
                        SWATCHES[index][2],
                        255,
                    ),
                    None,
                    &swatch,
                );
                continue;
            }
            if id == "colors" {
                // The disclosure shows the colour it will paint with.
                scene.fill(
                    Fill::NonZero,
                    transform,
                    Color::from_rgba8(self.color[0], self.color[1], self.color[2], 255),
                    None,
                    &vello::kurbo::Circle::new(r.center(), 8.0),
                );
                continue;
            }
            let icon = match id {
                "cancel" => Icon::X,
                "undo" => Icon::ArrowLeft,
                "redo" | "tool-arrow" => Icon::ArrowRight,
                "tool" => match self.tool {
                    MarkupTool::Pen => Icon::Pencil,
                    MarkupTool::Highlighter => Icon::PenTool,
                    MarkupTool::Arrow => Icon::ArrowRight,
                    MarkupTool::Rectangle => Icon::Square,
                    MarkupTool::Redact => Icon::Ban,
                },
                "tool-pen" => Icon::Pencil,
                "tool-highlighter" => Icon::PenTool,
                "tool-rectangle" => Icon::Square,
                "tool-redact" => Icon::Ban,
                "fit" => Icon::Maximize2,
                "open" => Icon::ExternalLink,
                "copy" => Icon::Copy,
                "save" => Icon::Download,
                _ => continue,
            };
            let icon_rect = if id == "save" {
                Rect::from_center_size(Point::new(r.x0 + 18.0, r.center().y), (14.0, 14.0))
            } else {
                r.inset(-10.0)
            };
            icons::draw_icon(
                scene,
                transform,
                icon_rect,
                icon,
                if id == "save" {
                    c.intent.on_accent
                } else {
                    c.text_primary
                },
                1.5,
            );
            if id == "save" {
                let (_, save_h) = tr.measure_style(
                    "Save",
                    None,
                    &theme
                        .text(TextRole::Caption)
                        .with_weight(TextWeight::Semibold),
                );
                tr.draw_style(
                    scene,
                    "Save",
                    c.intent.on_accent,
                    transform.pre_translate(
                        (r.x0 + 30.0, r.center().y - f64::from(save_h) / 2.0).into(),
                    ),
                    None,
                    &theme
                        .text(TextRole::Caption)
                        .with_weight(TextWeight::Semibold),
                );
            }
        }
        if let Some(on) = self.keyboard_control()
            && let Some((_, rect)) = self.controls().into_iter().find(|(id, _)| *id == on)
        {
            crate::widget::activation::paint_focus_ring(
                scene,
                transform,
                rect,
                rect.height() / 2.0,
                theme,
            );
        }
        if let Some((id, anchor)) = self
            .controls()
            .into_iter()
            .find(|(_, rect)| rect.contains(self.cursor))
            .filter(|(id, _)| {
                !(*id == "tool" && self.tool_picker_open
                    || *id == "colors" && self.color_picker_open)
            })
            && let Some(label) = self.control_label_key(id).map(markup_text)
        {
            // Measured, not estimated from a character count: the estimate
            // both clipped wide labels and over-padded narrow ones.
            let tip_style = theme.text(TextRole::Caption);
            let (label_w, label_h) = tr.measure_style(
                &label,
                None,
                &tip_style.clone().with_weight(TextWeight::Medium),
            );
            let width = (f64::from(label_w) + 18.0).max(48.0);
            let tooltip_y = if anchor.y0 < self.toolbar.y0 {
                anchor.y0 - 17.0
            } else {
                self.toolbar.y0 - 18.0
            };
            let tooltip =
                Rect::from_center_size(Point::new(anchor.center().x, tooltip_y), (width, 26.0));
            scene.fill(
                Fill::NonZero,
                transform,
                c.surfaces.raised,
                None,
                &RoundedRect::from_rect(tooltip, 13.0),
            );
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                crate::tokens::colors::Colors::hairline_on(c.surfaces.raised, 26),
                None,
                &RoundedRect::from_rect(tooltip.inflate(-0.5, -0.5), 12.5),
            );
            tr.draw_style(
                scene,
                &label,
                c.text_primary,
                transform.pre_translate(
                    (
                        tooltip.x0 + 9.0,
                        tooltip.center().y - f64::from(label_h) / 2.0,
                    )
                        .into(),
                ),
                None,
                &tip_style.clone().with_weight(TextWeight::Medium),
            );
        }
        scene.pop_layer();
        self.canvas = settled_canvas;
        self.toolbar = settled_toolbar;
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                if self.move_active_gesture(ctx.logical(position)) {
                    *ctx.handled = true;
                }
            }
            WindowEvent::MouseWheel { delta, .. } if self.viewport.contains(self.cursor) => {
                let amount = match delta {
                    MouseScrollDelta::LineDelta(_, y) => f64::from(*y),
                    MouseScrollDelta::PixelDelta(position) => position.y / ctx.scale_factor / 48.0,
                }
                .clamp(-4.0, 4.0);
                self.set_zoom_around(self.zoom * 1.18_f64.powf(amount), self.cursor);
                *ctx.handled = true;
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Middle,
                ..
            } if self.viewport.contains(self.cursor) => {
                self.panning = Some(self.cursor);
                *ctx.handled = true;
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if let Some((id, _)) = self
                    .controls()
                    .into_iter()
                    .find(|(_, r)| r.contains(self.cursor))
                {
                    self.press(id);
                    *ctx.handled = true;
                } else if self.tool_picker_open || self.color_picker_open {
                    self.close_picker();
                    *ctx.handled = true;
                } else if self.viewport.contains(self.cursor) && self.canvas.contains(self.cursor) {
                    let (width_px, alpha) = match self.tool {
                        MarkupTool::Highlighter => (18.0, 92),
                        MarkupTool::Pen | MarkupTool::Arrow | MarkupTool::Rectangle => (4.0, 255),
                        MarkupTool::Redact => (0.0, 255),
                    };
                    let mut color = self.color;
                    color = if self.tool == MarkupTool::Redact {
                        [18, 18, 22, 255]
                    } else {
                        color
                    };
                    color[3] = alpha;
                    self.drawing = Some(MarkupStroke {
                        tool: self.tool,
                        color,
                        width_px,
                        points: vec![self.image_point(self.cursor)],
                    });
                    *ctx.handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                if let Some(stroke) = self.drawing.take() {
                    self.strokes.push(stroke);
                    self.redo.clear();
                    *ctx.handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Middle,
                ..
            } => {
                if self.panning.take().is_some() {
                    *ctx.handled = true;
                }
            }
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                let command =
                    ctx.modifiers.state().control_key() || ctx.modifiers.state().super_key();
                match &event.logical_key {
                    Key::Named(NamedKey::Escape) => {
                        if self.tool_picker_open || self.color_picker_open {
                            self.close_picker();
                        } else {
                            self.pending.push(ScreenshotMarkupCommand::Cancel);
                        }
                        *ctx.handled = true;
                    }
                    Key::Named(NamedKey::Tab) if !command && !ctx.modifiers.state().alt_key() => {
                        self.move_keyboard(ctx.modifiers.state().shift_key());
                        *ctx.handled = true;
                    }
                    Key::Named(NamedKey::Enter | NamedKey::Space)
                        if self.keyboard_control().is_some() =>
                    {
                        if let Some(on) = self.keyboard_control() {
                            self.press(on);
                        }
                        *ctx.handled = true;
                    }
                    Key::Named(NamedKey::Enter) => {
                        self.pending
                            .push(ScreenshotMarkupCommand::Save(self.flattened()));
                        *ctx.handled = true;
                    }
                    Key::Character(ch) if command && ch.eq_ignore_ascii_case("z") => {
                        if ctx.modifiers.state().shift_key() {
                            if let Some(stroke) = self.redo.pop() {
                                self.strokes.push(stroke);
                            }
                        } else if let Some(stroke) = self.strokes.pop() {
                            self.redo.push(stroke);
                        }
                        *ctx.handled = true;
                    }
                    Key::Character(ch) if command && ch.eq_ignore_ascii_case("y") => {
                        if let Some(stroke) = self.redo.pop() {
                            self.strokes.push(stroke);
                        }
                        *ctx.handled = true;
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "ScreenshotMarkup"
    }
    fn test_id(&self) -> Option<&str> {
        Some("screenshot-markup")
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
    fn wants_pointer_capture(&self) -> bool {
        self.drawing.is_some() || self.panning.is_some()
    }
    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        if self.controls().iter().any(|(_, rect)| rect.contains(point)) {
            Some(winit::window::CursorIcon::Pointer)
        } else if self.viewport.contains(point) {
            Some(if self.panning.is_some() {
                winit::window::CursorIcon::Grabbing
            } else {
                winit::window::CursorIcon::Crosshair
            })
        } else {
            None
        }
    }
    fn traps_focus(&self) -> bool {
        true
    }
    fn is_focusable(&self) -> bool {
        true
    }
    fn is_default_keyboard_focus(&self) -> bool {
        true
    }
    fn set_focus(&mut self, focused: bool) {
        self.focused = focused;
        if !focused {
            self.keyboard = None;
        }
    }
    fn is_focused(&self) -> bool {
        self.focused
    }
    /// Tab walks the editor's own toolbar, which it paints itself.
    fn wants_tab_input(&self) -> bool {
        true
    }
    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        let keyboard = self.keyboard_control();
        let children = self
            .semantic_controls()
            .into_iter()
            .map(|(id, rect, key)| {
                let role = if id.starts_with("tool-") || id.starts_with("color-") {
                    Role::RadioButton
                } else {
                    Role::Button
                };
                let mut state = AccessState::new()
                    .focusable(true)
                    .focused(keyboard == Some(id));
                match id {
                    "tool" => state = state.expanded(self.tool_picker_open),
                    "colors" => state = state.expanded(self.color_picker_open),
                    _ => {}
                }
                if let Some(tool) = choice_tool(id) {
                    state = state.checked(self.tool == tool);
                } else if let Some(index) = SWATCH_IDS.iter().position(|swatch| *swatch == id) {
                    state = state.checked(self.color == SWATCHES[index]);
                }
                AccessNode::new(role)
                    .with_name(markup_text(key))
                    .with_action_target(id)
                    .with_state(state)
                    .with_bounds(rect)
            })
            .collect();
        Some(
            AccessNode::new(Role::Dialog)
                .with_name(markup_text("widget-markup-title"))
                .with_state(
                    AccessState::new()
                        .modal(true)
                        .focusable(true)
                        .focused(self.focused),
                )
                .with_children(children),
        )
    }
    fn synthetic_accessibility_rect(&self, path: &[usize]) -> Option<Rect> {
        let [index] = path else { return None };
        self.semantic_controls()
            .get(*index)
            .map(|(_, rect, _)| *rect)
    }
    fn focus_synthetic_accessibility(&mut self, path: &[usize]) -> bool {
        let [index] = path else { return false };
        let Some((id, _, _)) = self.semantic_controls().get(*index).copied() else {
            return false;
        };
        self.keyboard = Some(id);
        true
    }
    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &crate::a11y::Action,
        _state: &mut crate::widget::Edits<'_>,
        _path: &crate::widget::ModelPath,
    ) -> bool {
        use crate::a11y::Action;
        let Some(id) = self
            .controls()
            .into_iter()
            .map(|(id, _)| id)
            .find(|id| *id == target)
        else {
            return false;
        };
        let open = match id {
            "tool" => Some(self.tool_picker_open),
            "colors" => Some(self.color_picker_open),
            _ => None,
        };
        let applies = match action {
            Action::Activate => true,
            Action::ToggleCheck => id.starts_with("tool-") || id.starts_with("color-"),
            Action::Expand => open == Some(false),
            Action::Collapse => open == Some(true),
            _ => false,
        };
        if applies {
            self.press(id);
        }
        applies
    }
    fn needs_animation_frame(&self) -> bool {
        self.entrance_progress_override.is_none()
            && crate::time::now().saturating_duration_since(self.opened_at) < markup_entrance_time()
    }
    fn test_rects(&self) -> Vec<(String, Rect)> {
        self.controls()
            .into_iter()
            .map(|(id, r)| (format!("markup.{id}"), r))
            .collect()
    }
}

/// A string from the toolkit's catalogue.
fn markup_text(key: &str) -> String {
    crate::i18n::widget_format(key, &[])
}

/// The tool a picker choice selects.
fn choice_tool(id: &str) -> Option<MarkupTool> {
    Some(match id {
        "tool-pen" => MarkupTool::Pen,
        "tool-highlighter" => MarkupTool::Highlighter,
        "tool-arrow" => MarkupTool::Arrow,
        "tool-rectangle" => MarkupTool::Rectangle,
        "tool-redact" => MarkupTool::Redact,
        _ => return None,
    })
}

fn raster_stroke(rgba: &mut [u8], width: u32, height: u32, stroke: &MarkupStroke) {
    if stroke.points.is_empty() || width == 0 || height == 0 {
        return;
    }
    if stroke.tool == MarkupTool::Redact {
        if stroke.points.len() < 2 {
            return;
        }
        let start = stroke.points[0];
        let end = *stroke.points.last().unwrap_or(&start);
        let min_x = start.x.min(end.x).floor().clamp(0.0, f64::from(width)) as u32;
        let max_x = (start.x.max(end.x).ceil().max(0.0) as u32)
            .saturating_add(1)
            .min(width);
        let min_y = start.y.min(end.y).floor().clamp(0.0, f64::from(height)) as u32;
        let max_y = (start.y.max(end.y).ceil().max(0.0) as u32)
            .saturating_add(1)
            .min(height);
        for y in min_y..max_y {
            for x in min_x..max_x {
                let i = (y as usize * width as usize + x as usize) * 4;
                rgba[i..i + 4].copy_from_slice(&stroke.color);
                rgba[i + 3] = 255;
            }
        }
        return;
    }
    let radius = f64::from(stroke.width_px) / 2.0;
    let mut stamp = |center: Point| {
        let min_x = (center.x - radius).floor().max(0.0) as u32;
        let max_x = (center.x + radius).ceil().min(f64::from(width - 1)) as u32;
        let min_y = (center.y - radius).floor().max(0.0) as u32;
        let max_y = (center.y + radius).ceil().min(f64::from(height - 1)) as u32;
        for y in min_y..=max_y {
            for x in min_x..=max_x {
                let dx = f64::from(x) + 0.5 - center.x;
                let dy = f64::from(y) + 0.5 - center.y;
                if dx * dx + dy * dy > radius * radius {
                    continue;
                }
                let i = (y as usize * width as usize + x as usize) * 4;
                let a = f32::from(stroke.color[3]) / 255.0;
                for channel in 0..3 {
                    rgba[i + channel] = (f32::from(stroke.color[channel]) * a
                        + f32::from(rgba[i + channel]) * (1.0 - a))
                        .round() as u8;
                }
                rgba[i + 3] = 255;
            }
        }
    };
    stamp(stroke.points[0]);
    let mut segments: Vec<(Point, Point)> =
        if stroke.tool == MarkupTool::Arrow && stroke.points.len() > 1 {
            let start = stroke.points[0];
            let end = *stroke.points.last().unwrap_or(&start);
            let mut segments = vec![(start, end)];
            if let Some((left, right)) = arrow_head(start, end, f64::from(stroke.width_px)) {
                segments.extend([(left, end), (end, right)]);
            }
            segments
        } else if stroke.tool == MarkupTool::Rectangle && stroke.points.len() > 1 {
            let points = rectangle_points(
                stroke.points[0],
                *stroke.points.last().unwrap_or(&stroke.points[0]),
            );
            vec![
                (points[0], points[1]),
                (points[1], points[2]),
                (points[2], points[3]),
                (points[3], points[0]),
            ]
        } else {
            stroke
                .points
                .windows(2)
                .map(|pair| (pair[0], pair[1]))
                .collect()
        };
    for (a, b) in segments.drain(..) {
        let distance = a.distance(b);
        let steps = (distance / radius.max(0.75)).ceil().max(1.0) as u32;
        for step in 1..=steps {
            let t = f64::from(step) / f64::from(steps);
            stamp(Point::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t));
        }
    }
}

fn arrow_head(start: Point, end: Point, width_px: f64) -> Option<(Point, Point)> {
    let delta = end - start;
    let distance = delta.hypot();
    if distance < 0.5 {
        return None;
    }
    let unit = delta / distance;
    let perpendicular = Vec2::new(-unit.y, unit.x);
    let length = (14.0 + width_px * 1.5).min(distance * 0.45);
    let base = end - unit * length;
    let spread = perpendicular * (length * 0.52);
    Some((base + spread, base - spread))
}

fn rectangle_points(start: Point, end: Point) -> [Point; 4] {
    let x0 = start.x.min(end.x);
    let x1 = start.x.max(end.x);
    let y0 = start.y.min(end.y);
    let y1 = start.y.max(end.y);
    [
        Point::new(x0, y0),
        Point::new(x1, y0),
        Point::new(x1, y1),
        Point::new(x0, y1),
    ]
}

#[cfg(test)]
#[path = "screenshot_markup_a11y_tests.rs"]
mod a11y_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flattened_export_changes_only_annotated_pixels() {
        {
            let original: Arc<[u8]> = Arc::from(vec![20_u8; 16 * 16 * 4]);
            let mut markup = ScreenshotMarkup::new(original.clone(), 16, 16);
            markup.strokes.push(MarkupStroke {
                tool: MarkupTool::Pen,
                color: [255, 0, 0, 255],
                width_px: 2.0,
                points: vec![Point::new(4.0, 4.0), Point::new(12.0, 12.0)],
            });
            let export = markup.flattened();
            assert_eq!((export.width, export.height), (16, 16));
            assert_eq!(export.rgba.len(), original.len());
            assert_ne!(export.rgba, original);
            assert_eq!(&export.rgba[0..4], &original[0..4]);
        }

        {
            let original: Arc<[u8]> = Arc::from(vec![100_u8, 100, 100, 255]);
            let mut markup = ScreenshotMarkup::new(original, 1, 1);
            markup.strokes.push(MarkupStroke {
                tool: MarkupTool::Highlighter,
                color: [255, 200, 0, 92],
                width_px: 18.0,
                points: vec![Point::new(0.5, 0.5)],
            });
            let export = markup.flattened();
            assert!(export.rgba[0] > 100 && export.rgba[0] < 255);
            assert!(export.rgba[1] > 100 && export.rgba[1] < 200);
            assert_eq!(export.rgba[3], 255);
        }

        {
            let original: Arc<[u8]> = Arc::from(vec![0_u8; 64 * 64 * 4]);
            let stroke = |tool| MarkupStroke {
                tool,
                color: [255, 80, 80, 255],
                width_px: 3.0,
                points: vec![Point::new(10.0, 32.0), Point::new(50.0, 32.0)],
            };
            let mut pen = ScreenshotMarkup::new(original.clone(), 64, 64);
            pen.strokes.push(stroke(MarkupTool::Pen));
            let mut arrow = ScreenshotMarkup::new(original, 64, 64);
            arrow.strokes.push(stroke(MarkupTool::Arrow));
            let changed =
                |rgba: &Arc<[u8]>| rgba.chunks_exact(4).filter(|pixel| pixel[3] != 0).count();

            assert!(changed(&arrow.flattened().rgba) > changed(&pen.flattened().rgba));
        }

        {
            let original: Arc<[u8]> = Arc::from(vec![0_u8; 64 * 64 * 4]);
            let mut markup = ScreenshotMarkup::new(original, 64, 64);
            markup.strokes.push(MarkupStroke {
                tool: MarkupTool::Rectangle,
                color: [80, 160, 255, 255],
                width_px: 3.0,
                points: vec![Point::new(10.0, 10.0), Point::new(50.0, 50.0)],
            });
            let export = markup.flattened();
            let left_edge = (45_usize * 64 + 10) * 4;
            let bottom_edge = (50_usize * 64 + 45) * 4;

            assert_eq!(export.rgba[left_edge + 3], 255);
            assert_eq!(export.rgba[bottom_edge + 3], 255);
        }

        {
            let original: Arc<[u8]> = Arc::from(vec![180_u8; 32 * 32 * 4]);
            let mut markup = ScreenshotMarkup::new(original.clone(), 32, 32);
            markup.strokes.push(MarkupStroke {
                tool: MarkupTool::Redact,
                color: [18, 18, 22, 255],
                width_px: 0.0,
                points: vec![Point::new(8.0, 9.0), Point::new(20.0, 18.0)],
            });
            let export = markup.flattened();
            let inside = (12_usize * 32 + 12) * 4;
            let outside = (7_usize * 32 + 7) * 4;

            assert_eq!(&export.rgba[inside..inside + 4], &[18, 18, 22, 255]);
            assert_eq!(
                &export.rgba[outside..outside + 4],
                &original[outside..outside + 4]
            );
        }
    }

    #[test]
    fn strokes_undo_redo_animate_and_extend_exactly() {
        {
            let original: Arc<[u8]> = Arc::from(vec![0_u8; 4 * 4 * 4]);
            let mut markup = ScreenshotMarkup::new(original, 4, 4);
            let stroke = MarkupStroke {
                tool: MarkupTool::Pen,
                color: [255, 0, 0, 255],
                width_px: 2.0,
                points: vec![Point::new(1.0, 1.0), Point::new(2.0, 2.0)],
            };
            markup.strokes.push(stroke.clone());
            markup.activate("undo");
            assert!(markup.strokes.is_empty());
            assert_eq!(markup.redo, vec![stroke.clone()]);
            markup.activate("redo");
            assert_eq!(markup.strokes, vec![stroke]);
            assert!(markup.redo.is_empty());
        }

        {
            let original: Arc<[u8]> = Arc::from(vec![0_u8; 16 * 9 * 4]);
            let spawn = Rect::new(1_200.0, 18.0, 1_246.0, 42.0);
            let mut markup = ScreenshotMarkup::new(original, 16, 9).with_spawn_from(Some(spawn));
            markup.rect = Rect::new(0.0, 0.0, 1_600.0, 900.0);
            markup.layout_parts();
            let settled = markup.canvas;

            markup.set_entrance_transition_progress(0);
            assert_eq!(markup.entrance_motion(), 0.0);
            assert_eq!(markup.presentation_canvas(0.0), spawn);
            markup.set_entrance_transition_progress(1_000);
            assert_eq!(markup.entrance_motion(), 1.0);
            let end = markup.presentation_canvas(1.0);
            assert!((end.x0 - settled.x0).abs() < 0.001);
            assert!((end.y0 - settled.y0).abs() < 0.001);
            assert!((end.x1 - settled.x1).abs() < 0.001);
            assert!((end.y1 - settled.y1).abs() < 0.001);
            assert!(!markup.needs_animation_frame());
        }

        {
            let original: Arc<[u8]> = Arc::from(vec![0_u8; 100 * 80 * 4]);
            let mut markup = ScreenshotMarkup::new(original, 100, 80);
            markup.rect = Rect::new(0.0, 0.0, 1_000.0, 700.0);
            markup.layout_parts();
            markup.drawing = Some(MarkupStroke {
                tool: MarkupTool::Pen,
                color: SWATCHES[0],
                width_px: 4.0,
                points: vec![Point::new(10.0, 10.0)],
            });

            assert!(markup.move_active_gesture(markup.canvas.center()));
            assert!(markup.panning.is_none());
            assert_eq!(markup.drawing.as_ref().unwrap().points.len(), 2);
        }
    }

    #[test]
    fn zoom_and_fit_track_source_pixels_without_changing_export() {
        {
            let original: Arc<[u8]> = Arc::from(vec![33_u8; 100 * 80 * 4]);
            let mut markup = ScreenshotMarkup::new(original.clone(), 100, 80);
            markup.rect = Rect::new(0.0, 0.0, 1_000.0, 700.0);
            markup.layout_parts();
            let anchor = Point::new(markup.viewport.x0 + 180.0, markup.viewport.y0 + 120.0);
            let before = markup.image_point(anchor);
            markup.set_zoom_around(3.0, anchor);
            let after = markup.image_point(anchor);

            assert!((before.x - after.x).abs() < 0.001);
            assert!((before.y - after.y).abs() < 0.001);
            assert_eq!(markup.flattened().rgba, original);

            markup.pan = Vec2::new(100_000.0, -100_000.0);
            markup.clamp_pan();
            assert!(markup.canvas.x0 <= markup.viewport.x0);
            assert!(markup.canvas.x1 >= markup.viewport.x1);
            assert!(markup.canvas.y0 <= markup.viewport.y0);
            assert!(markup.canvas.y1 >= markup.viewport.y1);
            markup.reset_view();
            assert_eq!(markup.zoom, 1.0);
            assert_eq!(markup.canvas, markup.fit_canvas);
        }

        {
            let small: Arc<[u8]> = Arc::from(vec![0_u8; 100 * 80 * 4]);
            let mut markup = ScreenshotMarkup::new(small, 100, 80);
            markup.rect = Rect::new(0.0, 0.0, 1_000.0, 700.0);
            markup.layout_parts();
            assert_eq!(markup.fit_canvas.width(), 100.0);
            assert_eq!(markup.fit_canvas.height(), 80.0);
            assert!((markup.display_scale() - 1.0).abs() < 0.001);

            let large: Arc<[u8]> = Arc::from(vec![0_u8; 1_000 * 2_000 * 4]);
            let mut markup = ScreenshotMarkup::new(large, 1_000, 2_000);
            markup.rect = Rect::new(0.0, 0.0, 1_000.0, 700.0);
            markup.layout_parts();
            assert!(markup.display_scale() < 1.0);
            markup.set_zoom_around(2.0, markup.canvas.center());
            assert!((markup.display_scale() - markup.fit_canvas.width() / 500.0).abs() < 0.001);
        }
    }

    fn markup_at(width: f64) -> ScreenshotMarkup {
        let rgba: Arc<[u8]> = Arc::from(vec![20_u8; 64 * 64 * 4]);
        let mut markup = ScreenshotMarkup::new(rgba, 64, 64);
        markup.rect = Rect::new(0.0, 0.0, width, 700.0);
        markup.layout_parts();
        markup
    }

    fn assert_no_two_controls_overlap(controls: &[(&'static str, Rect)]) {
        for (i, (a_id, a)) in controls.iter().enumerate() {
            for (b_id, b) in controls.iter().skip(i + 1) {
                let overlap = a.intersect(*b);
                assert!(
                    overlap.is_zero_area(),
                    "{a_id} {a:?} and {b_id} {b:?} claim the same pixels: {overlap:?}"
                );
            }
        }
    }

    #[test]
    fn markup_toolbar_lays_out_tokenised_targets_that_collapse_when_narrow() {
        // Case `toolbar_controls_meet_the_minimum_target_and_never_overlap`.
        // Every toolbar control is at least the minimum pointer target and no
        // two of them share a pixel — the swatches used to be 30 px targets.
        {
            let markup = markup_at(1200.0);
            let controls = markup.controls();
            for (id, r) in &controls {
                assert!(
                    r.width() >= MIN_TARGET - 1e-6 && r.height() >= MIN_TARGET - 1e-6,
                    "{id} {r:?} is under the {MIN_TARGET}px minimum target"
                );
            }
            assert_eq!(
                controls
                    .iter()
                    .filter(|(id, _)| id.starts_with("color-"))
                    .count(),
                SWATCHES.len(),
                "every swatch fits on a wide toolbar"
            );
            assert_no_two_controls_overlap(&controls);
        }

        // Case `narrow_toolbar_collapses_swatches_behind_a_colours_button`.
        // On a surface too narrow for the whole strip the swatches drop from
        // the right instead of sliding under the action buttons; the actions
        // and the tool group are always offered.
        {
            let mut markup = markup_at(440.0);
            let controls = markup.controls();
            assert_no_two_controls_overlap(&controls);
            for required in [
                "cancel", "undo", "redo", "tool", "fit", "colors", "open", "copy", "save",
            ] {
                assert!(
                    controls.iter().any(|(id, _)| *id == required),
                    "{required} must survive a narrow toolbar"
                );
            }
            assert!(
                !controls.iter().any(|(id, _)| id.starts_with("color-")),
                "a 420px toolbar cannot hold five swatch targets; they collapse behind `colors`"
            );
            let open = controls
                .iter()
                .find(|(id, _)| *id == "open")
                .map(|(_, r)| *r)
                .expect("open is offered");
            for (id, r) in &controls {
                if *id == "colors" {
                    assert!(
                        r.width() >= MIN_TARGET && r.x1 <= open.x0,
                        "{id} {r:?} is a full target and stays clear of open {open:?}"
                    );
                }
            }

            // Opening the disclosure makes every colour reachable again — by
            // pointer and by its `markup.color-*` target — without any control
            // overlapping another.
            markup.activate("colors");
            let opened = markup.controls();
            assert_no_two_controls_overlap(&opened);
            for id in SWATCH_IDS {
                let r = opened
                    .iter()
                    .find(|(candidate, _)| *candidate == id)
                    .map(|(_, r)| *r)
                    .unwrap_or_else(|| panic!("{id} is reachable from the open palette"));
                assert!(
                    r.width() >= MIN_TARGET && r.height() >= MIN_TARGET,
                    "{id} {r:?} is under the minimum target"
                );
                assert!(
                    markup.rect.x0 <= r.x0 && r.x1 <= markup.rect.x1,
                    "{id} {r:?} leaves the surface {:?}",
                    markup.rect
                );
            }
            markup.activate("color-green");
            assert_eq!(markup.color, SWATCHES[3], "the palette choice is applied");
            assert!(
                !markup.color_picker_open,
                "choosing a colour closes the palette"
            );
        }

        // Case `narrow_redact_badge_stays_clear_of_the_actions`.
        // With the redact tool active on a narrow toolbar the "Opaque" badge
        // shrinks to the room before the actions instead of running under them.
        {
            let mut markup = markup_at(440.0);
            markup.tool = MarkupTool::Redact;
            let controls = markup.controls();
            let open = controls
                .iter()
                .find(|(id, _)| *id == "open")
                .map(|(_, r)| *r)
                .expect("open is offered");
            let badge = markup.redact_badge_rect();
            assert!(
                badge.width() == 0.0 || badge.x1 <= open.x0 - CONTROL_GAP + 1e-6,
                "badge {badge:?} runs under open {open:?}"
            );
            for (id, r) in &controls {
                let overlap = badge.intersect(*r);
                assert!(
                    overlap.is_zero_area(),
                    "badge {badge:?} and {id} {r:?} claim the same pixels"
                );
            }
        }

        // Case `layout_constants_equal_the_spacing_tokens`.
        // The layout constants stand in for tokens because the toolbar is
        // laid out before `draw` sees a theme; pin them so a token retune
        // surfaces here rather than drifting silently.
        {
            let spacing = crate::tokens::Spacing::default();
            assert!(
                (CANVAS_INSET - f64::from(spacing.s6)).abs() < 1e-9,
                "CANVAS_INSET is s6"
            );
            assert!(
                (HEADER_GAP - f64::from(spacing.s2)).abs() < 1e-9,
                "HEADER_GAP is s2"
            );
            assert!(
                (CONTROL_GAP - f64::from(spacing.s1)).abs() < 1e-9,
                "CONTROL_GAP is s1"
            );
            assert!(
                (TOOLBAR_INSET - f64::from(spacing.s2)).abs() < 1e-9,
                "TOOLBAR_INSET is s2"
            );
        }

        // Case `badge_and_picker_follow_the_placed_tool_group`.
        // The redact badge and the tool picker are derived from the placed
        // controls, not from a literal sum of their neighbours' widths.
        {
            let mut markup = markup_at(1200.0);
            markup.tool = MarkupTool::Redact;
            let controls = markup.controls();
            let fit = controls
                .iter()
                .find(|(id, _)| *id == "fit")
                .map(|(_, r)| *r)
                .expect("fit is offered");
            let tool = controls
                .iter()
                .find(|(id, _)| *id == "tool")
                .map(|(_, r)| *r)
                .expect("tool is offered");
            let badge = markup.redact_badge_rect();
            assert!(
                (badge.x0 - (fit.x1 + CONTROL_GAP)).abs() < 1e-6,
                "badge {badge:?} does not start one gap after fit {fit:?}"
            );
            assert!(
                (badge.center().y - fit.center().y).abs() < 1e-6,
                "badge shares the toolbar's centre line"
            );
            let picker = markup.tool_picker_rect();
            assert!(
                (picker.x0 + 24.0 - tool.center().x).abs() < 1e-6,
                "the first picker choice sits above the tool button"
            );
        }
    }
}
