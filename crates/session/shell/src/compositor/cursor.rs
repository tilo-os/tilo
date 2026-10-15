//! One source of truth for the pointer image.
//!
//! Compositor-drawn shapes are vector geometry in a 24-unit design space,
//! rasterised with supersampling. A client that supplies its own cursor
//! surface is converted into the same premultiplied bitmap. DRM cursor planes,
//! the software fallback and captures all consume that one bitmap, so changing
//! render paths never changes the pointer's silhouette or hotspot.

use std::sync::{Arc, Mutex, OnceLock};

use tilo_lock::MutexRecover as _;
use ui::ColorCompat;
use vello::Scene;
use vello::kurbo::{Affine, Rect};
use vello::peniko::{Blob, Color, Fill, ImageBrush};

/// Cursor ink comes from the shared dark desktop theme rather than bypassing
/// the design-token contract. The system pointer deliberately keeps the same
/// light fill and dark outline across themes so it stays identifiable over
/// arbitrary client content, which the shell theme does not control.
pub(crate) fn palette() -> (Color, Color) {
    let colors = &ui::theme::Theme::dark().tokens.colors;
    (colors.text_primary, colors.surfaces.base)
}

/// Largest legacy KMS cursor plane supported by every backend we target,
/// and the largest logical cursor size the accessibility setting offers.
pub const PLANE_SIZE: u32 = 64;
/// Largest cursor, in pixels, the compositor rasterises: a large
/// accessibility cursor on a high-density panel, on a cursor plane that
/// takes it.
pub const MAX_PIXEL_SIZE: u32 = 256;
/// Side of the square grid the cursor outlines are authored on.
const DESIGN_SIZE: u32 = 24;
const SUPERSAMPLE: u32 = 4;

/// Shapes the compositor draws itself: its own default, what its chrome
/// asks for, and what a client names through `wp_cursor_shape_v1`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum CursorShape {
    #[default]
    Default,
    Text,
    Pointer,
    Grab,
    Grabbing,
    NotAllowed,
    Wait,
    Progress,
    Crosshair,
    Help,
    ContextMenu,
    Move,
    ResizeN,
    ResizeS,
    ResizeE,
    ResizeW,
    ResizeNe,
    ResizeNw,
    ResizeSe,
    ResizeSw,
    ResizeEw,
    ResizeNs,
    ResizeNesw,
    ResizeNwse,
}

impl CursorShape {
    /// Every shape, for exhaustive rendering checks.
    pub const ALL: [Self; 24] = [
        Self::Default,
        Self::Text,
        Self::Pointer,
        Self::Grab,
        Self::Grabbing,
        Self::NotAllowed,
        Self::Wait,
        Self::Progress,
        Self::Crosshair,
        Self::Help,
        Self::ContextMenu,
        Self::Move,
        Self::ResizeN,
        Self::ResizeS,
        Self::ResizeE,
        Self::ResizeW,
        Self::ResizeNe,
        Self::ResizeNw,
        Self::ResizeSe,
        Self::ResizeSw,
        Self::ResizeEw,
        Self::ResizeNs,
        Self::ResizeNesw,
        Self::ResizeNwse,
    ];

    /// The drawn shape for a CSS/`wp_cursor_shape_v1` cursor name. Names
    /// without a dedicated drawing fall back to their closest relative
    /// rather than to an arrow that says nothing about the action.
    pub fn from_icon(icon: winit::window::CursorIcon) -> Self {
        use winit::window::CursorIcon as I;
        match icon {
            I::Text | I::VerticalText => Self::Text,
            I::Pointer => Self::Pointer,
            I::Grab => Self::Grab,
            I::Grabbing => Self::Grabbing,
            I::NotAllowed | I::NoDrop => Self::NotAllowed,
            I::Wait => Self::Wait,
            I::Progress => Self::Progress,
            I::Crosshair | I::Cell => Self::Crosshair,
            I::Help | I::DndAsk => Self::Help,
            I::ContextMenu => Self::ContextMenu,
            I::Move | I::AllScroll | I::AllResize => Self::Move,
            I::NResize => Self::ResizeN,
            I::SResize => Self::ResizeS,
            I::EResize => Self::ResizeE,
            I::WResize => Self::ResizeW,
            I::NeResize => Self::ResizeNe,
            I::NwResize => Self::ResizeNw,
            I::SeResize => Self::ResizeSe,
            I::SwResize => Self::ResizeSw,
            I::EwResize | I::ColResize => Self::ResizeEw,
            I::NsResize | I::RowResize => Self::ResizeNs,
            I::NeswResize => Self::ResizeNesw,
            I::NwseResize => Self::ResizeNwse,
            _ => Self::Default,
        }
    }

    /// The standard cursor name this drawing stands for, for a host that
    /// draws the pointer itself (the nested development backend).
    pub fn icon(self) -> winit::window::CursorIcon {
        use winit::window::CursorIcon as I;
        match self {
            Self::Default => I::Default,
            Self::Text => I::Text,
            Self::Pointer => I::Pointer,
            Self::Grab => I::Grab,
            Self::Grabbing => I::Grabbing,
            Self::NotAllowed => I::NotAllowed,
            Self::Wait => I::Wait,
            Self::Progress => I::Progress,
            Self::Crosshair => I::Crosshair,
            Self::Help => I::Help,
            Self::ContextMenu => I::ContextMenu,
            Self::Move => I::Move,
            Self::ResizeN => I::NResize,
            Self::ResizeS => I::SResize,
            Self::ResizeE => I::EResize,
            Self::ResizeW => I::WResize,
            Self::ResizeNe => I::NeResize,
            Self::ResizeNw => I::NwResize,
            Self::ResizeSe => I::SeResize,
            Self::ResizeSw => I::SwResize,
            Self::ResizeEw => I::EwResize,
            Self::ResizeNs => I::NsResize,
            Self::ResizeNesw => I::NeswResize,
            Self::ResizeNwse => I::NwseResize,
        }
    }

    /// Stable lowercase name, used by the state dump.
    pub fn name(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Text => "text",
            Self::Pointer => "pointer",
            Self::Grab => "grab",
            Self::Grabbing => "grabbing",
            Self::NotAllowed => "not-allowed",
            Self::Wait => "wait",
            Self::Progress => "progress",
            Self::Crosshair => "crosshair",
            Self::Help => "help",
            Self::ContextMenu => "context-menu",
            Self::Move => "move",
            Self::ResizeN => "n-resize",
            Self::ResizeS => "s-resize",
            Self::ResizeE => "e-resize",
            Self::ResizeW => "w-resize",
            Self::ResizeNe => "ne-resize",
            Self::ResizeNw => "nw-resize",
            Self::ResizeSe => "se-resize",
            Self::ResizeSw => "sw-resize",
            Self::ResizeEw => "ew-resize",
            Self::ResizeNs => "ns-resize",
            Self::ResizeNesw => "nesw-resize",
            Self::ResizeNwse => "nwse-resize",
        }
    }

    /// Where the pointer position sits on the shape, in design units.
    fn hotspot(self) -> (f64, f64) {
        match self {
            Self::Default | Self::Progress | Self::Help | Self::ContextMenu => (0.0, 0.0),
            Self::Pointer => (9.25, 1.0),
            _ => (12.0, 12.0),
        }
    }
}

/// What the pointer currently shows.
#[derive(Clone, Debug)]
pub enum PointerImage {
    /// A compositor-drawn shape, sized from the accessibility cursor size.
    Shape(CursorShape),
    /// The focused client hid the pointer (`set_cursor` with no surface, or
    /// a cursor surface without a buffer).
    Hidden,
    /// The focused client's own cursor surface, already converted.
    Client(Arc<CursorBitmap>),
}

impl Default for PointerImage {
    fn default() -> Self {
        Self::Shape(CursorShape::Default)
    }
}

impl PartialEq for PointerImage {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Shape(a), Self::Shape(b)) => a == b,
            (Self::Hidden, Self::Hidden) => true,
            (Self::Client(a), Self::Client(b)) => Arc::ptr_eq(a, b),
            _ => false,
        }
    }
}

impl PointerImage {
    /// The pixels to show at `size` (the accessibility cursor size, which
    /// only compositor-drawn shapes follow), or `None` when hidden.
    pub fn bitmap(&self, size: u32) -> Option<Arc<CursorBitmap>> {
        match self {
            Self::Shape(shape) => Some(bitmap(*shape, size)),
            Self::Hidden => None,
            Self::Client(bitmap) => Some(Arc::clone(bitmap)),
        }
    }

    /// Machine-readable description for the state dump.
    pub fn describe(&self) -> serde_json::Value {
        match self {
            Self::Shape(shape) => serde_json::json!({ "kind": "shape", "shape": shape.name() }),
            Self::Hidden => serde_json::json!({ "kind": "hidden" }),
            Self::Client(bitmap) => serde_json::json!({
                "kind": "surface",
                "width": bitmap.width,
                "height": bitmap.height,
                "hotspot": [bitmap.hotspot.0, bitmap.hotspot.1],
            }),
        }
    }
}

/// Premultiplied RGBA cursor pixels and their explicit hotspot.
#[derive(Clone, Debug)]
pub struct CursorBitmap {
    pub width: u32,
    pub height: u32,
    pub hotspot: (u32, u32),
    rgba: Arc<Vec<u8>>,
}

impl CursorBitmap {
    pub fn rgba(&self) -> &[u8] {
        &self.rgba
    }

    /// Convert a client's cursor buffer (premultiplied RGBA, as `wl_shm`
    /// ARGB8888 is) to output pixels. The buffer holds `density` pixels per
    /// logical pixel (its buffer scale, or the ratio its viewport sets) and
    /// the hotspot is surface-local logical coordinates; both are resampled
    /// to `output_scale` and bounded to the cursor plane, preserving the
    /// aspect ratio. `None` for an empty or truncated buffer.
    pub fn from_client(
        rgba: &[u8],
        width: u32,
        height: u32,
        hotspot: (i32, i32),
        density: f64,
        output_scale: f64,
    ) -> Option<Self> {
        let expected = usize::try_from(width.checked_mul(height)?.checked_mul(4)?).ok()?;
        if width == 0 || height == 0 || rgba.len() < expected {
            return None;
        }
        let positive = |value: f64| {
            if value.is_finite() && value > 0.0 {
                value
            } else {
                1.0
            }
        };
        let (density, output_scale) = (positive(density), positive(output_scale));
        let mut factor = output_scale / density;
        let longest = f64::from(width.max(height)) * factor;
        if longest > f64::from(PLANE_SIZE) {
            factor *= f64::from(PLANE_SIZE) / longest;
        }
        let out_w = scaled_dimension(width, factor);
        let out_h = scaled_dimension(height, factor);
        let mut out =
            vec![0_u8; usize::try_from(out_w.saturating_mul(out_h).saturating_mul(4)).ok()?];
        for (index, pixel) in out.chunks_exact_mut(4).enumerate() {
            let index = u32::try_from(index).unwrap_or(u32::MAX);
            let (x, y) = (
                index.checked_rem(out_w).unwrap_or(0),
                index.checked_div(out_w).unwrap_or(0),
            );
            let sx = source_coordinate(x, factor, width);
            let sy = source_coordinate(y, factor, height);
            let offset = usize::try_from(
                sy.saturating_mul(width)
                    .saturating_add(sx)
                    .saturating_mul(4),
            )
            .unwrap_or(usize::MAX);
            if let Some(source) = pixel_at(rgba, offset) {
                pixel.copy_from_slice(&source);
            }
        }
        // The hotspot goes where its pixel went: a cursor shrunk to fit the
        // plane takes its hotspot along, or the drawn tip leaves the point.
        let scale_hotspot = |value: i32, limit: u32| {
            let scaled = (f64::from(value.max(0)) * density * factor).round();
            clamp_to_u32(scaled).min(limit.saturating_sub(1))
        };
        Some(Self {
            width: out_w,
            height: out_h,
            hotspot: (
                scale_hotspot(hotspot.0, out_w),
                scale_hotspot(hotspot.1, out_h),
            ),
            rgba: Arc::new(out),
        })
    }
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the value is rounded and clamped to the u32 range before the cast"
)]
fn clamp_to_u32(value: f64) -> u32 {
    if value.is_nan() {
        return 0;
    }
    value.clamp(0.0, f64::from(u32::MAX)) as u32
}

fn scaled_dimension(source: u32, factor: f64) -> u32 {
    clamp_to_u32((f64::from(source) * factor).round()).clamp(1, PLANE_SIZE)
}

/// Nearest source texel for output coordinate `out` under `factor`.
fn source_coordinate(out: u32, factor: f64, limit: u32) -> u32 {
    clamp_to_u32(((f64::from(out) + 0.5) / factor).floor()).min(limit.saturating_sub(1))
}

type Cache = Vec<((CursorShape, u32), Arc<CursorBitmap>)>;

/// The cursor's side in output pixels: its logical `size` at the output's
/// `scale`, so a pointer is as big on a 2x panel as on a 1x monitor and is
/// drawn sharp at that density rather than enlarged or left tiny.
pub fn pixel_size(size: u32, scale: f64) -> u32 {
    let scale = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    clamp_to_u32((f64::from(size) * scale).round()).clamp(1, MAX_PIXEL_SIZE)
}

/// Rasterise once per shape/size from the vector outlines; `size` is in
/// output pixels (see [`pixel_size`]).
pub fn bitmap(shape: CursorShape, size: u32) -> Arc<CursorBitmap> {
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    let size = size.clamp(16, MAX_PIXEL_SIZE);
    let cache = CACHE.get_or_init(|| Mutex::new(Vec::new()));
    let mut cache = cache.lock_recover();
    if let Some((_, found)) = cache.iter().find(|(key, _)| *key == (shape, size)) {
        return Arc::clone(found);
    }
    let rendered = Arc::new(rasterise(shape, size));
    cache.push(((shape, size), Arc::clone(&rendered)));
    rendered
}

/// Paint a cursor bitmap into a scene with its hotspot at `at`. Capture uses
/// the very pixels the cursor plane shows rather than a second rendition.
pub fn draw_scene(scene: &mut Scene, cursor: &CursorBitmap, at: vello::kurbo::Point) {
    if cursor.width == 0 || cursor.height == 0 {
        return;
    }
    let data: Arc<dyn AsRef<[u8]> + Send + Sync> = cursor.rgba.clone();
    let image = ui::raster::rgba8_premultiplied(Blob::new(data), cursor.width, cursor.height);
    let origin = (
        at.x - f64::from(cursor.hotspot.0),
        at.y - f64::from(cursor.hotspot.1),
    );
    scene.fill(
        Fill::NonZero,
        Affine::translate(origin),
        &ImageBrush::new(image),
        None,
        &Rect::new(0.0, 0.0, f64::from(cursor.width), f64::from(cursor.height)),
    );
}

type Point2 = (f64, f64);

/// One filled region: an outer contour minus its holes.
struct Part {
    outer: Vec<Point2>,
    holes: Vec<Vec<Point2>>,
}

impl Part {
    fn solid(outer: Vec<Point2>) -> Self {
        Self {
            outer,
            holes: Vec::new(),
        }
    }

    fn with_holes(outer: Vec<Point2>, holes: Vec<Vec<Point2>>) -> Self {
        Self { outer, holes }
    }

    fn contains(&self, point: Point2) -> bool {
        point_in_polygon(point, &self.outer)
            && !self.holes.iter().any(|hole| point_in_polygon(point, hole))
    }

    fn edge_distance(&self, point: Point2) -> f64 {
        self.holes
            .iter()
            .map(|hole| polygon_distance(point, hole))
            .fold(polygon_distance(point, &self.outer), f64::min)
    }

    fn map(self, f: impl Fn(Point2) -> Point2) -> Self {
        Self {
            outer: self.outer.into_iter().map(&f).collect(),
            holes: self
                .holes
                .into_iter()
                .map(|hole| hole.into_iter().map(&f).collect())
                .collect(),
        }
    }
}

fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<Point2> {
    vec![(x0, y0), (x1, y0), (x1, y1), (x0, y1)]
}

fn arc_points(center: Point2, radius: f64, from: f64, to: f64, steps: u32) -> Vec<Point2> {
    (0..=steps)
        .map(|step| {
            let t = f64::from(step) / f64::from(steps.max(1));
            let angle = from + (to - from) * t;
            (
                center.0 + radius * angle.cos(),
                center.1 + radius * angle.sin(),
            )
        })
        .collect()
}

fn circle(center: Point2, radius: f64) -> Vec<Point2> {
    let mut points = arc_points(center, radius, 0.0, std::f64::consts::TAU, 32);
    points.pop();
    points
}

/// A stadium: the segment `a`→`b` thickened by `radius` with round ends.
fn capsule(a: Point2, b: Point2, radius: f64) -> Vec<Point2> {
    use std::f64::consts::{FRAC_PI_2, PI};
    let heading = (b.1 - a.1).atan2(b.0 - a.0);
    let mut points = arc_points(b, radius, heading - FRAC_PI_2, heading + FRAC_PI_2, 8);
    points.extend(arc_points(
        a,
        radius,
        heading + FRAC_PI_2,
        heading + PI + FRAC_PI_2,
        8,
    ));
    points
}

fn rounded_rect(x0: f64, y0: f64, x1: f64, y1: f64, radius: f64) -> Vec<Point2> {
    use std::f64::consts::{FRAC_PI_2, PI};
    let mut points = arc_points((x1 - radius, y0 + radius), radius, -FRAC_PI_2, 0.0, 5);
    points.extend(arc_points(
        (x1 - radius, y1 - radius),
        radius,
        0.0,
        FRAC_PI_2,
        5,
    ));
    points.extend(arc_points(
        (x0 + radius, y1 - radius),
        radius,
        FRAC_PI_2,
        PI,
        5,
    ));
    points.extend(arc_points(
        (x0 + radius, y0 + radius),
        radius,
        PI,
        PI + FRAC_PI_2,
        5,
    ));
    points
}

/// A thick arc band between `from` and `to` (radians, y down).
fn arc_band(center: Point2, radius: f64, width: f64, from: f64, to: f64) -> Vec<Point2> {
    let mut points = arc_points(center, radius + width / 2.0, from, to, 16);
    let mut inner = arc_points(center, radius - width / 2.0, from, to, 16);
    inner.reverse();
    points.extend(inner);
    points
}

fn rotate_about(center: Point2, angle: f64) -> impl Fn(Point2) -> Point2 {
    let (sin, cos) = angle.sin_cos();
    move |(x, y)| {
        let (dx, dy) = (x - center.0, y - center.1);
        (
            center.0 + dx * cos - dy * sin,
            center.1 + dx * sin + dy * cos,
        )
    }
}

fn default_arrow() -> Vec<Point2> {
    // A compact, slightly softened modern arrow. The tip is deliberately
    // (0,0), matching the explicit hotspot and existing pointer coordinates.
    vec![
        (1.0, 0.75),
        (1.2, 19.15),
        (5.8, 14.75),
        (9.65, 22.25),
        (13.25, 20.35),
        (9.45, 13.1),
        (16.05, 12.7),
    ]
}

/// A two-headed arrow through (12, 12), pointing along `angle`.
fn double_arrow(angle: f64) -> Part {
    // Design-grid units: half the length, head length, head half-width
    // and shaft half-width.
    let (half, head, head_half_width, shaft) = (9.0, 5.0, 4.5, 1.3);
    let local = vec![
        (-half, 0.0),
        (-half + head, -head_half_width),
        (-half + head, -shaft),
        (half - head, -shaft),
        (half - head, -head_half_width),
        (half, 0.0),
        (half - head, head_half_width),
        (half - head, shaft),
        (-half + head, shaft),
        (-half + head, head_half_width),
    ];
    Part::solid(local)
        .map(|(x, y)| (x + 12.0, y + 12.0))
        .map(rotate_about((12.0, 12.0), angle))
}

/// An arrow towards an edge along `angle`, stopped by a bar marking it.
fn edge_arrow(angle: f64) -> Vec<Part> {
    let arrow = vec![
        (-7.0, -1.3),
        (1.0, -1.3),
        (1.0, -4.5),
        (6.0, 0.0),
        (1.0, 4.5),
        (1.0, 1.3),
        (-7.0, 1.3),
    ];
    let bar = rect(6.9, -6.0, 8.9, 6.0);
    [arrow, bar]
        .into_iter()
        .map(|points| {
            Part::solid(points)
                .map(|(x, y)| (x + 11.0, y + 12.0))
                .map(rotate_about((12.0, 12.0), angle))
        })
        .collect()
}

fn hourglass() -> Vec<Part> {
    let glass = vec![
        (7.5, 4.0),
        (16.5, 4.0),
        (16.5, 6.0),
        (13.2, 12.0),
        (16.5, 18.0),
        (16.5, 20.0),
        (7.5, 20.0),
        (7.5, 18.0),
        (10.8, 12.0),
        (7.5, 6.0),
    ];
    // The emptied upper bulb reads as time having passed.
    let emptied = vec![(9.6, 5.9), (14.4, 5.9), (12.0, 10.3)];
    vec![
        Part::with_holes(glass, vec![emptied]),
        Part::solid(rect(6.0, 2.0, 18.0, 4.5)),
        Part::solid(rect(6.0, 19.5, 18.0, 22.0)),
    ]
}

fn open_hand(fingers: &[(Point2, Point2, f64)], palm: Vec<Point2>) -> Vec<Part> {
    let mut parts = vec![Part::solid(palm)];
    parts.extend(
        fingers
            .iter()
            .map(|&(a, b, radius)| Part::solid(capsule(a, b, radius))),
    );
    parts
}

fn geometry(shape: CursorShape) -> Vec<Part> {
    use std::f64::consts::{FRAC_PI_2, FRAC_PI_4, PI};
    match shape {
        CursorShape::Default => vec![Part::solid(default_arrow())],
        CursorShape::Text => vec![
            Part::solid(rect(10.7, 4.0, 13.3, 20.0)),
            Part::solid(rect(8.0, 2.7, 16.0, 5.3)),
            Part::solid(rect(8.0, 18.7, 16.0, 21.3)),
        ],
        CursorShape::Pointer => open_hand(
            &[
                ((9.25, 2.75), (9.25, 13.0), 1.75),
                ((12.6, 9.3), (12.6, 13.0), 1.6),
                ((15.3, 10.1), (15.3, 13.5), 1.5),
                ((17.7, 11.2), (17.7, 14.0), 1.35),
                ((4.6, 13.4), (8.6, 18.2), 1.55),
            ],
            rounded_rect(7.5, 11.5, 19.05, 21.5, 3.0),
        ),
        CursorShape::Grab => open_hand(
            &[
                ((8.3, 5.5), (8.3, 12.0), 1.55),
                ((11.3, 4.2), (11.3, 12.0), 1.55),
                ((14.3, 4.6), (14.3, 12.0), 1.55),
                ((17.1, 6.3), (17.1, 12.0), 1.45),
                ((3.8, 10.2), (7.5, 15.5), 1.5),
            ],
            rounded_rect(6.5, 10.0, 18.5, 20.5, 3.2),
        ),
        CursorShape::Grabbing => open_hand(
            &[
                ((8.3, 8.6), (8.3, 10.0), 1.55),
                ((11.3, 8.0), (11.3, 10.0), 1.55),
                ((14.3, 8.2), (14.3, 10.0), 1.55),
                ((17.0, 9.0), (17.0, 10.5), 1.45),
                ((5.2, 12.0), (9.5, 14.6), 1.45),
            ],
            rounded_rect(6.5, 9.5, 18.5, 20.5, 3.2),
        ),
        CursorShape::NotAllowed => vec![
            Part::with_holes(circle((12.0, 12.0), 8.5), vec![circle((12.0, 12.0), 5.5)]),
            Part::solid(rect(5.0, 10.5, 19.0, 13.5)).map(rotate_about((12.0, 12.0), FRAC_PI_4)),
        ],
        CursorShape::Wait => hourglass(),
        CursorShape::Progress => {
            let mut parts = vec![Part::solid(default_arrow())];
            parts.extend(hourglass().into_iter().map(|part| {
                Part::solid(part.outer)
                    .map(|(x, y)| (19.5 + (x - 12.0) * 0.45, 18.5 + (y - 12.0) * 0.45))
            }));
            parts
        }
        CursorShape::Crosshair => vec![
            Part::solid(rect(10.3, 2.0, 13.7, 9.0)),
            Part::solid(rect(10.3, 15.0, 13.7, 22.0)),
            Part::solid(rect(2.0, 10.3, 9.0, 13.7)),
            Part::solid(rect(15.0, 10.3, 22.0, 13.7)),
        ],
        CursorShape::Help => vec![
            Part::solid(default_arrow()),
            Part::solid(arc_band((19.6, 15.2), 2.5, 1.9, PI, PI + 1.5 * PI)),
            Part::solid(rect(18.65, 17.2, 20.55, 19.3)),
            Part::solid(circle((19.6, 21.6), 1.15)),
        ],
        CursorShape::ContextMenu => vec![
            Part::solid(default_arrow()),
            Part::with_holes(
                rect(14.5, 14.0, 23.0, 23.0),
                vec![
                    rect(16.3, 16.0, 21.2, 16.9),
                    rect(16.3, 18.3, 21.2, 19.2),
                    rect(16.3, 20.6, 21.2, 21.5),
                ],
            ),
        ],
        CursorShape::Move => vec![double_arrow(0.0), double_arrow(FRAC_PI_2)],
        CursorShape::ResizeE => edge_arrow(0.0),
        CursorShape::ResizeSe => edge_arrow(FRAC_PI_4),
        CursorShape::ResizeS => edge_arrow(FRAC_PI_2),
        CursorShape::ResizeSw => edge_arrow(FRAC_PI_2 + FRAC_PI_4),
        CursorShape::ResizeW => edge_arrow(PI),
        CursorShape::ResizeNw => edge_arrow(PI + FRAC_PI_4),
        CursorShape::ResizeN => edge_arrow(PI + FRAC_PI_2),
        CursorShape::ResizeNe => edge_arrow(-FRAC_PI_4),
        CursorShape::ResizeEw => vec![double_arrow(0.0)],
        CursorShape::ResizeNs => vec![double_arrow(FRAC_PI_2)],
        CursorShape::ResizeNwse => vec![double_arrow(FRAC_PI_4)],
        CursorShape::ResizeNesw => vec![double_arrow(-FRAC_PI_4)],
    }
}

fn rasterise(shape: CursorShape, size: u32) -> CursorBitmap {
    let supersampled = size.saturating_mul(SUPERSAMPLE);
    let high = rasterise_cpu(shape, supersampled);
    let rgba = downsample_premultiplied(&high, supersampled, size);
    let scale = f64::from(size) / f64::from(DESIGN_SIZE);
    let (hx, hy) = shape.hotspot();
    let hotspot = |value: f64| clamp_to_u32((value * scale).round()).min(size.saturating_sub(1));
    CursorBitmap {
        width: size,
        height: size,
        hotspot: (hotspot(hx), hotspot(hy)),
        rgba: Arc::new(rgba),
    }
}

/// Classify every supersampled pixel as fill, outline or empty. Parts are a
/// union: an edge that lies deep inside another part is an internal seam and
/// takes the fill colour, so overlapping pieces read as one silhouette.
fn rasterise_cpu(shape: CursorShape, size: u32) -> Vec<u8> {
    let mut rgba =
        vec![0_u8; usize::try_from(size.saturating_mul(size).saturating_mul(4)).unwrap_or(0)];
    let scale = f64::from(size) / f64::from(DESIGN_SIZE);
    let parts: Vec<Part> = geometry(shape)
        .into_iter()
        .map(|part| part.map(|(x, y)| (x * scale, y * scale)))
        .collect();
    // The outline is part of the drawing: it grows with the cursor, so a
    // pointer at 2x is the 1x one at twice the density, not a thinner one.
    let stroke_radius =
        (f64::from(SUPERSAMPLE) * 0.85 * (scale / f64::from(SUPERSAMPLE)).max(1.0)).max(1.0);
    let (fill, outline_color) = palette();
    for y in 0..size {
        for x in 0..size {
            let point = (f64::from(x) + 0.5, f64::from(y) + 0.5);
            let mut inside = false;
            let mut near_edge = false;
            let mut deep_inside = false;
            for part in &parts {
                let distance = part.edge_distance(point);
                let contained = part.contains(point);
                inside |= contained;
                near_edge |= distance <= stroke_radius;
                deep_inside |= contained && distance > stroke_radius;
            }
            let (color, alpha) = if near_edge && !deep_inside {
                (
                    [outline_color.r(), outline_color.g(), outline_color.b()],
                    outline_color.a(),
                )
            } else if inside {
                ([fill.r(), fill.g(), fill.b()], fill.a())
            } else {
                ([0_u8, 0, 0], 0_u8)
            };
            let offset = y.saturating_mul(size).saturating_add(x).saturating_mul(4);
            let offset = usize::try_from(offset).unwrap_or(usize::MAX);
            if let Some(pixel) = rgba.get_mut(offset..offset.saturating_add(4)) {
                let [r, g, b] = color;
                pixel.copy_from_slice(&[r, g, b, alpha]);
            }
        }
    }
    rgba
}

/// Each polygon vertex paired with the one before it, wrapping around.
fn edges(polygon: &[(f64, f64)]) -> impl Iterator<Item = ((f64, f64), (f64, f64))> + '_ {
    let previous = polygon.last().into_iter().chain(polygon.iter());
    polygon.iter().copied().zip(previous.copied())
}

fn point_in_polygon(point: (f64, f64), polygon: &[(f64, f64)]) -> bool {
    let mut inside = false;
    for (current, previous) in edges(polygon) {
        if (current.1 > point.1) != (previous.1 > point.1)
            && point.0
                < (previous.0 - current.0) * (point.1 - current.1) / (previous.1 - current.1)
                    + current.0
        {
            inside = !inside;
        }
    }
    inside
}

fn polygon_distance(point: (f64, f64), polygon: &[(f64, f64)]) -> f64 {
    edges(polygon)
        .map(|(current, previous)| segment_distance(point, previous, current))
        .fold(f64::INFINITY, f64::min)
}

fn segment_distance(point: (f64, f64), start: (f64, f64), end: (f64, f64)) -> f64 {
    let delta = (end.0 - start.0, end.1 - start.1);
    let length_squared = delta.0 * delta.0 + delta.1 * delta.1;
    if length_squared <= f64::EPSILON {
        return ((point.0 - start.0).powi(2) + (point.1 - start.1).powi(2)).sqrt();
    }
    let projection = (((point.0 - start.0) * delta.0 + (point.1 - start.1) * delta.1)
        / length_squared)
        .clamp(0.0, 1.0);
    let nearest = (
        start.0 + projection * delta.0,
        start.1 + projection * delta.1,
    );
    ((point.0 - nearest.0).powi(2) + (point.1 - nearest.1).powi(2)).sqrt()
}

/// The four bytes at `offset`, when the whole pixel lies inside `buffer`.
fn pixel_at(buffer: &[u8], offset: usize) -> Option<[u8; 4]> {
    buffer.get(offset..offset.checked_add(4)?)?.try_into().ok()
}

/// `channel * alpha / 255`, the premultiplication of one 8-bit channel.
fn premultiply(channel: u8, alpha: u8) -> u32 {
    u32::from(channel).saturating_mul(u32::from(alpha)) / 255
}

fn downsample_premultiplied(src: &[u8], source_size: u32, size: u32) -> Vec<u8> {
    let mut out =
        vec![0_u8; usize::try_from(size.saturating_mul(size).saturating_mul(4)).unwrap_or(0)];
    let ss = usize::try_from(SUPERSAMPLE).unwrap_or(1);
    let source_size = usize::try_from(source_size).unwrap_or(0);
    let size = usize::try_from(size).unwrap_or(0);
    let samples = u32::try_from(ss.saturating_mul(ss)).unwrap_or(1).max(1);
    for (index, dst) in out.chunks_exact_mut(4).enumerate() {
        let (dst_row, dst_column) = (
            index.checked_div(size).unwrap_or(0),
            index.checked_rem(size).unwrap_or(0),
        );
        let mut sum = [0_u32; 4];
        for sy in 0..ss {
            for sx in 0..ss {
                let row = dst_row.saturating_mul(ss).saturating_add(sy);
                let column = dst_column.saturating_mul(ss).saturating_add(sx);
                let src_offset = row
                    .saturating_mul(source_size)
                    .saturating_add(column)
                    .saturating_mul(4);
                let Some([red, green, blue, alpha]) = pixel_at(src, src_offset) else {
                    continue;
                };
                let [red_sum, green_sum, blue_sum, alpha_sum] = &mut sum;
                *red_sum = red_sum.saturating_add(premultiply(red, alpha));
                *green_sum = green_sum.saturating_add(premultiply(green, alpha));
                *blue_sum = blue_sum.saturating_add(premultiply(blue, alpha));
                *alpha_sum = alpha_sum.saturating_add(u32::from(alpha));
            }
        }
        for (byte, total) in dst.iter_mut().zip(sum) {
            *byte = u8::try_from(total.checked_div(samples).unwrap_or(0).min(255)).unwrap_or(255);
        }
    }
    out
}

/// Visits every visible cursor pixel that lands inside a 32-bit `width` x
/// `height` buffer, with the cursor drawn so its hotspot sits at `(px, py)`.
/// `paint` gets the cursor's premultiplied RGBA and the destination pixel.
fn for_each_covered_pixel(
    dst: &mut [u8],
    pitch: usize,
    width: usize,
    height: usize,
    (px, py): (i32, i32),
    cursor: &CursorBitmap,
    mut paint: impl FnMut([u8; 4], &mut [u8; 4]),
) {
    let left = px.saturating_sub(i32::try_from(cursor.hotspot.0).unwrap_or(0));
    let top = py.saturating_sub(i32::try_from(cursor.hotspot.1).unwrap_or(0));
    let row_bytes = usize::try_from(cursor.width).map_or(usize::MAX, |w| w.saturating_mul(4));
    for (y, row) in (0_i32..).zip(cursor.rgba.chunks_exact(row_bytes)) {
        let Some(dy) = usize::try_from(top.saturating_add(y))
            .ok()
            .filter(|&dy| dy < height)
        else {
            continue;
        };
        for (x, source) in (0_i32..).zip(row.chunks_exact(4)) {
            let Some(dx) = usize::try_from(left.saturating_add(x))
                .ok()
                .filter(|&dx| dx < width)
            else {
                continue;
            };
            let Ok(source) = <[u8; 4]>::try_from(source) else {
                continue;
            };
            let offset = dy
                .saturating_mul(pitch)
                .saturating_add(dx.saturating_mul(4));
            if let Some(Ok(pixel)) = dst
                .get_mut(offset..offset.saturating_add(4))
                .map(<&mut [u8; 4]>::try_from)
                && source[3] != 0
            {
                paint(source, pixel);
            }
        }
    }
}

/// Alpha-composite the shared premultiplied bitmap into an XRGB8888 buffer.
pub fn composite_xrgb(
    dst: &mut [u8],
    pitch: usize,
    width: usize,
    height: usize,
    px: i32,
    py: i32,
    cursor: &CursorBitmap,
) {
    for_each_covered_pixel(
        dst,
        pitch,
        width,
        height,
        (px, py),
        cursor,
        |[red, green, blue, alpha], pixel| {
            let inverse = 255_u8.saturating_sub(alpha);
            let over = |source: u8, under: u8| {
                u8::try_from(
                    u32::from(source)
                        .saturating_add(premultiply(under, inverse))
                        .min(255),
                )
                .unwrap_or(u8::MAX)
            };
            // XRGB is little-endian B,G,R,X; the source is premultiplied RGBA.
            let [dst_blue, dst_green, dst_red, _] = *pixel;
            *pixel = [
                over(blue, dst_blue),
                over(green, dst_green),
                over(red, dst_red),
                0xff,
            ];
        },
    );
}

/// Composite the software cursor into an HDR `XBGR2101010` buffer holding a
/// PQ BT.2020 signal. The cursor's colour goes through the output's SDR → PQ
/// encoder, so it has the brightness of the rest of the desktop; its
/// antialiased edge blends in signal space, which is exact at the fully
/// opaque and fully clear pixels that make up almost all of it.
pub fn composite_xbgr2101010(
    dst: &mut [u8],
    pitch: usize,
    width: usize,
    height: usize,
    at: (i32, i32),
    cursor: &CursorBitmap,
    encoder: &crate::shell::color_pipeline::SdrToPqEncoder,
) {
    for_each_covered_pixel(
        dst,
        pitch,
        width,
        height,
        at,
        cursor,
        |[red, green, blue, alpha], word| {
            // The bitmap is premultiplied; the encoder wants straight colour.
            let straight = |channel: u8| {
                u8::try_from(
                    u32::from(channel)
                        .saturating_mul(255)
                        .checked_div(u32::from(alpha))
                        .unwrap_or(0)
                        .min(255),
                )
                .unwrap_or(u8::MAX)
            };
            let source = encoder.encode([straight(red), straight(green), straight(blue)]);
            let under = u32::from_le_bytes(*word);
            let under = [under & 0x3FF, (under >> 10) & 0x3FF, (under >> 20) & 0x3FF];
            let inverse = 255_u32.saturating_sub(u32::from(alpha));
            let blended = [0, 1, 2].map(|channel| {
                let over = source.get(channel).copied().map_or(0, u32::from);
                let below = under.get(channel).copied().unwrap_or(0);
                let mixed = over
                    .saturating_mul(u32::from(alpha))
                    .saturating_add(below.saturating_mul(inverse))
                    / 255;
                u16::try_from(mixed).unwrap_or(1023)
            });
            *word = crate::shell::color_pipeline::pack_xbgr2101010(blended).to_le_bytes();
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opaque_bounds(bitmap: &CursorBitmap) -> Option<(u32, u32, u32, u32)> {
        let mut bounds: Option<(u32, u32, u32, u32)> = None;
        for (index, pixel) in bitmap.rgba().chunks_exact(4).enumerate() {
            if pixel[3] == 0 {
                continue;
            }
            let index = u32::try_from(index).unwrap();
            let (x, y) = (
                index.checked_rem(bitmap.width).unwrap(),
                index.checked_div(bitmap.width).unwrap(),
            );
            bounds = Some(bounds.map_or((x, y, x, y), |(x0, y0, x1, y1)| {
                (x0.min(x), y0.min(y), x1.max(x), y1.max(y))
            }));
        }
        bounds
    }

    #[test]
    fn hdr_cursor_is_encoded_at_desktop_brightness_and_clipped() {
        let encoder = crate::shell::color_pipeline::SdrToPqEncoder::new(203.0);
        let cursor = CursorBitmap {
            width: 2,
            height: 1,
            hotspot: (0, 0),
            // Opaque white, then half-covered black (premultiplied).
            rgba: Arc::new(vec![255, 255, 255, 255, 0, 0, 0, 128]),
        };
        let background = crate::shell::color_pipeline::pack_xbgr2101010([400, 400, 400]);
        let mut dst: Vec<u8> = background.to_le_bytes().repeat(4);
        // A 2x2 target, cursor at the bottom-right corner so it is clipped.
        composite_xbgr2101010(&mut dst, 8, 2, 2, (0, 0), &cursor, &encoder);
        let word = |index: usize| {
            u32::from_le_bytes([
                dst[index * 4],
                dst[index * 4 + 1],
                dst[index * 4 + 2],
                dst[index * 4 + 3],
            ])
        };
        assert_eq!(word(0), encoder.encode_xbgr2101010([255, 255, 255]));
        let half = word(1) & 0x3FF;
        assert!(
            (195..=205).contains(&half),
            "half black over 400 → ~200, got {half}"
        );
        assert_eq!(word(2), background, "untouched below the cursor");
        let mut clipped: Vec<u8> = background.to_le_bytes().repeat(4);
        composite_xbgr2101010(&mut clipped, 8, 2, 2, (1, 1), &cursor, &encoder);
        assert_eq!(
            u32::from_le_bytes([clipped[12], clipped[13], clipped[14], clipped[15]]),
            encoder.encode_xbgr2101010([255, 255, 255])
        );
        assert_eq!(&clipped[..12], &background.to_le_bytes().repeat(3)[..]);
    }

    #[test]
    fn cursor_images_are_cached_antialiased_and_composited_with_clipping() {
        // Phase `default_cursor_is_cached_and_antialiased_at_accessibility_sizes`.
        for size in [24, 48, 64] {
            let first = bitmap(CursorShape::Default, size);
            let second = bitmap(CursorShape::Default, size);
            assert!(Arc::ptr_eq(&first, &second));
            assert_eq!((first.width, first.height), (size, size));
            assert_eq!(first.hotspot, (0, 0));
            assert_eq!(first.rgba.len(), usize::try_from(size * size * 4).unwrap());
            assert!(first.rgba.chunks_exact(4).any(|pixel| pixel[3] == 255));
            assert!(
                first
                    .rgba
                    .chunks_exact(4)
                    .any(|pixel| (1..=254).contains(&pixel[3]))
            );
            assert!(first.rgba.chunks_exact(4).any(|pixel| pixel[3] == 0));
        }
        // Phase `software_composite_clips_and_preserves_uncovered_pixels`.
        {
            let cursor = bitmap(CursorShape::Default, 24);
            let mut dst = vec![17_u8; 12 * 12 * 4];
            composite_xrgb(&mut dst, 12 * 4, 12, 12, -3, -3, &cursor);
            assert!(dst.chunks_exact(4).any(|pixel| pixel != [17, 17, 17, 17]));
            assert_eq!(&dst[(11 * 12 + 11) * 4..][..4], &[17, 17, 17, 17]);
        }
    }

    #[test]
    fn every_shape_rasterises_with_fill_outline_and_its_hotspot_on_the_ink() {
        let (fill, outline) = palette();
        for shape in CursorShape::ALL {
            for size in [24, 48] {
                let cursor = bitmap(shape, size);
                let pixels = cursor.rgba();
                // Thin strokes blend fill and outline at 24 px, so judge
                // opaque pixels by which ink they are closer to.
                let midpoint = u16::midpoint(u16::from(fill.r()), u16::from(outline.r()));
                let light = |p: &[u8]| u16::from(p[0]) > midpoint;
                assert!(
                    pixels
                        .chunks_exact(4)
                        .any(|p| p[3] == 255 && light(p) == (fill.r() > outline.r())),
                    "{shape:?}@{size} shows no fill ink"
                );
                assert!(
                    pixels
                        .chunks_exact(4)
                        .any(|p| p[3] == 255 && light(p) == (outline.r() > fill.r())),
                    "{shape:?}@{size} shows no outline ink"
                );
                let (x0, y0, x1, y1) = opaque_bounds(&cursor).expect("visible ink");
                let covered = pixels.chunks_exact(4).filter(|p| p[3] > 0).count();
                assert!(
                    covered * 10 < pixels.len() / 4 * 7,
                    "{shape:?}@{size} flooded the plane ({covered} pixels)"
                );
                // The hotspot lies within (or right beside, for the arrow
                // tip) the drawn silhouette, never in empty space far away.
                let (hx, hy) = cursor.hotspot;
                assert!(
                    hx + 2 >= x0 && hx <= x1 + 2 && hy + 2 >= y0 && hy <= y1 + 2,
                    "{shape:?}@{size} hotspot {:?} outside ink {:?}",
                    cursor.hotspot,
                    (x0, y0, x1, y1)
                );
            }
        }
    }

    #[test]
    fn centred_shapes_scale_their_hotspot_with_the_accessibility_size() {
        assert_eq!(bitmap(CursorShape::Text, 24).hotspot, (12, 12));
        assert_eq!(bitmap(CursorShape::Text, 48).hotspot, (24, 24));
        assert_eq!(bitmap(CursorShape::Pointer, 48).hotspot, (19, 2));
    }

    #[test]
    fn shapes_are_distinct_drawings() {
        let mut seen: Vec<(CursorShape, Arc<CursorBitmap>)> = Vec::new();
        for shape in CursorShape::ALL {
            let cursor = bitmap(shape, 24);
            for (other, drawn) in &seen {
                assert_ne!(
                    drawn.rgba(),
                    cursor.rgba(),
                    "{shape:?} draws exactly like {other:?}"
                );
            }
            seen.push((shape, cursor));
        }
    }

    #[test]
    fn named_icons_map_to_their_drawing() {
        use winit::window::CursorIcon as I;
        assert_eq!(CursorShape::from_icon(I::Text), CursorShape::Text);
        assert_eq!(CursorShape::from_icon(I::Pointer), CursorShape::Pointer);
        assert_eq!(CursorShape::from_icon(I::ColResize), CursorShape::ResizeEw);
        assert_eq!(CursorShape::from_icon(I::NoDrop), CursorShape::NotAllowed);
        assert_eq!(CursorShape::from_icon(I::Alias), CursorShape::Default);
    }

    #[test]
    fn drawn_cursors_are_rasterised_at_the_output_density() {
        assert_eq!(pixel_size(24, 1.0), 24);
        assert_eq!(pixel_size(24, 2.0), 48);
        assert_eq!(pixel_size(24, 1.5), 36);
        assert_eq!(pixel_size(24, 1.25), 30);
        assert_eq!(pixel_size(48, 2.0), 96);
        assert_eq!(pixel_size(24, f64::NAN), 24);
        assert_eq!(pixel_size(64, 8.0), MAX_PIXEL_SIZE);

        // The same vector arrow at 2x: twice the pixels, the hotspot on the
        // same point of the drawing, and an outline that grew with it rather
        // than a 1x hairline around a bigger fill.
        let one = bitmap(CursorShape::Pointer, 24);
        let two = bitmap(CursorShape::Pointer, 48);
        assert_eq!((two.width, two.height), (48, 48));
        for (at_two, at_one) in [
            (two.hotspot.0, one.hotspot.0),
            (two.hotspot.1, one.hotspot.1),
        ] {
            assert!(
                at_two.abs_diff(at_one.saturating_mul(2)) <= 1,
                "hotspot {:?} at 2x for {:?} at 1x",
                two.hotspot,
                one.hotspot
            );
        }
        let opaque = |cursor: &CursorBitmap| {
            cursor
                .rgba()
                .chunks_exact(4)
                .filter(|pixel| pixel[3] > 200)
                .count()
        };
        let dark = |cursor: &CursorBitmap| {
            cursor
                .rgba()
                .chunks_exact(4)
                .filter(|pixel| pixel[3] > 200 && pixel[0] < 96 && pixel[1] < 96 && pixel[2] < 96)
                .count()
        };
        let ratio = |a: usize, b: usize| {
            f64::from(u32::try_from(a).unwrap_or(u32::MAX))
                / f64::from(u32::try_from(b).unwrap_or(u32::MAX).max(1))
        };
        let area_ratio = ratio(opaque(&two), opaque(&one));
        assert!(
            (3.4..=4.6).contains(&area_ratio),
            "2x covers four times the pixels, got {area_ratio}"
        );
        let outline_ratio = ratio(dark(&two), dark(&one));
        assert!(
            outline_ratio > 3.0,
            "the outline scales with the drawing, got {outline_ratio}"
        );
        // Fractional densities rasterise exactly, not the nearest integer.
        assert_eq!(bitmap(CursorShape::Default, pixel_size(24, 1.25)).width, 30);
    }

    #[test]
    fn client_cursor_is_resampled_to_output_scale_with_its_hotspot() {
        // A 2x buffer for a 16-logical-pixel cursor, on a 1x output.
        let rgba: Vec<u8> = (0..32 * 32).flat_map(|_| [10, 20, 30, 255]).collect();
        let cursor = CursorBitmap::from_client(&rgba, 32, 32, (4, 6), 2.0, 1.0).unwrap();
        assert_eq!((cursor.width, cursor.height), (16, 16));
        assert_eq!(cursor.hotspot, (4, 6));
        assert!(
            cursor
                .rgba()
                .chunks_exact(4)
                .all(|p| p == [10, 20, 30, 255])
        );

        // The same logical cursor on a 1.5x output keeps its logical size.
        let cursor = CursorBitmap::from_client(&rgba, 32, 32, (4, 6), 2.0, 1.5).unwrap();
        assert_eq!((cursor.width, cursor.height), (24, 24));
        assert_eq!(cursor.hotspot, (6, 9));

        // Oversized cursors are bounded to the plane, aspect preserved.
        let wide: Vec<u8> = vec![255; 256 * 64 * 4];
        let cursor = CursorBitmap::from_client(&wide, 256, 64, (300, 300), 1.0, 1.0).unwrap();
        assert_eq!((cursor.width, cursor.height), (PLANE_SIZE, 16));
        assert!(cursor.hotspot.0 < cursor.width && cursor.hotspot.1 < cursor.height);

        assert!(CursorBitmap::from_client(&rgba[..10], 32, 32, (0, 0), 1.0, 1.0).is_none());
    }

    /// At 2x a big client cursor is shrunk to fit the plane; its hotspot
    /// must shrink with it, or the drawn tip is away from where clicks land.
    #[test]
    fn a_client_cursor_shrunk_to_the_plane_takes_its_hotspot_along() {
        // 96 logical pixels at 1x on a 2x output: 192 pixels, shrunk to 64.
        let mut rgba = vec![0_u8; 96 * 96 * 4];
        let tip = (24 * 96 + 24) * 4;
        rgba[tip..tip + 4].copy_from_slice(&[255, 255, 255, 255]);
        let cursor = CursorBitmap::from_client(&rgba, 96, 96, (24, 24), 1.0, 2.0).unwrap();
        assert_eq!((cursor.width, cursor.height), (PLANE_SIZE, PLANE_SIZE));
        assert_eq!(
            cursor.hotspot,
            (16, 16),
            "the same relative point of the image, not 2 * 24 logical"
        );
        let at =
            |x: u32, y: u32| cursor.rgba()[usize::try_from((y * cursor.width + x) * 4).unwrap()];
        assert_eq!(at(16, 16), 255, "the hotspot is on the drawn tip");

        // A fractionally scaled client: a 48-pixel buffer its viewport
        // declares 24 logical pixels big is 2x, and needs no shrinking at 2x.
        let viewported: Vec<u8> = vec![255; 48 * 48 * 4];
        let cursor = CursorBitmap::from_client(&viewported, 48, 48, (4, 6), 2.0, 2.0).unwrap();
        assert_eq!((cursor.width, cursor.height), (48, 48));
        assert_eq!(cursor.hotspot, (8, 12));
    }

    #[test]
    fn hidden_pointer_has_no_pixels() {
        assert!(PointerImage::Hidden.bitmap(24).is_none());
        assert_eq!(
            PointerImage::Hidden.describe(),
            serde_json::json!({ "kind": "hidden" })
        );
        assert_eq!(
            PointerImage::Shape(CursorShape::Text).describe()["shape"],
            "text"
        );
    }

    #[test]
    fn software_composite_honours_the_hotspot() {
        let cursor = bitmap(CursorShape::Crosshair, 24);
        let (w, h) = (48_usize, 48_usize);
        let mut dst = vec![0_u8; w * h * 4];
        composite_xrgb(&mut dst, w * 4, w, h, 24, 24, &cursor);
        // Crosshair bars are centred on the pointer: ink both left and right
        // of x = 24 on the pointer's row.
        let row = 24 * w * 4;
        assert!(dst[row..row + 20 * 4].chunks_exact(4).any(|p| p[3] == 0xff));
        assert!(
            dst[row + 28 * 4..row + 48 * 4]
                .chunks_exact(4)
                .any(|p| p[3] == 0xff)
        );
    }
}
