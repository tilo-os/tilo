//! The time-zone globe: the Earth drawn orthographically with the land, the night
//! side and every zone's city, turned by dragging for the time-zone picker.
//!
//! Points behind the front hemisphere continue outward along the same azimuth
//! (radius `2R − R·sin c`), so a coastline stays one closed, non-crossing ring
//! however the globe turns. The land is filled even-odd with a frame around it,
//! since a ring enclosing the point opposite the centre maps outside its image.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::LazyLock;

use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use ui::theme::Theme;
use ui::widget::{ModelPath, TaffyTreeExt, Widget};
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Circle, Point, Rect, Shape as _, Stroke};
use vello::peniko::{BlendMode, Fill};
use winit::event::{ElementState, MouseButton, WindowEvent};

use ui::i18n::ArgValue;

ui::embedded_catalog!(fn, ["en-US", "de-DE"]);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TimeZonePoint {
    pub id: String,
    pub region: String,
    pub city: String,
    /// From the installed tzdata `zone1970.tab`; microdegrees keep [`TimeZonePoint`]
    /// `Eq` and exact enough for a compact picker.
    pub latitude_e6: i32,
    pub longitude_e6: i32,
}

impl TimeZonePoint {
    #[must_use]
    pub fn latitude(&self) -> f64 {
        f64::from(self.latitude_e6) / 1_000_000.0
    }

    #[must_use]
    pub fn longitude(&self) -> f64 {
        f64::from(self.longitude_e6) / 1_000_000.0
    }
}

/// Natural Earth's 1:110m land polygons (`data/world-land.b64`, format and
/// provenance in `data/README.md`): tzdata says where each zone's city is, not
/// which coastline it sits on.
const WORLD_LAND: &str = include_str!("../data/world-land.b64");

/// `None` for padding and line breaks.
const fn sextet(c: u8) -> Option<u8> {
    match c {
        b'A'..=b'Z' => Some(c.wrapping_sub(b'A')),
        b'a'..=b'z' => Some(c.wrapping_sub(b'a').wrapping_add(26)),
        b'0'..=b'9' => Some(c.wrapping_sub(b'0').wrapping_add(52)),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

const fn decoded_len(mut text: &[u8]) -> usize {
    let mut digits = 0_usize;
    while let [c, rest @ ..] = text {
        if sextet(*c).is_some() {
            digits = digits.wrapping_add(1);
        }
        text = rest;
    }
    digits.wrapping_mul(3) / 4
}

/// The `N` bytes of standard, padded base64 `text`, decoded while compiling, so
/// the app carries the bytes and no decoder.
const fn decode_base64<const N: usize>(mut text: &[u8]) -> [u8; N] {
    let mut out = [0_u8; N];
    let mut slots: &mut [u8] = &mut out;
    let mut bits = 0_u32;
    let mut held = 0_u32;
    while let [c, rest @ ..] = text {
        text = rest;
        let Some(value) = sextet(*c) else {
            continue;
        };
        bits = bits.wrapping_shl(6) | u32::from_le_bytes([value, 0, 0, 0]);
        held = held.wrapping_add(6);
        if held >= 8 {
            held = held.wrapping_sub(8);
            let [byte, ..] = bits.wrapping_shr(held).to_le_bytes();
            bits &= 1_u32.wrapping_shl(held).wrapping_sub(1);
            if let [slot, tail @ ..] = slots {
                *slot = byte;
                slots = tail;
            }
        }
    }
    out
}

static WORLD_LAND_BYTES: [u8; decoded_len(WORLD_LAND.as_bytes())] =
    decode_base64(WORLD_LAND.as_bytes());

/// Walk `WORLD_LAND`: each ring is a little-endian `u16` point count, its first
/// point as two `i16` (longitude, latitude) in tenths of a degree, then one `i8`
/// pair per further point, the step from the point before.
pub fn walk_world_land(mut visit: impl FnMut(f64, f64, bool)) {
    let mut bytes = WORLD_LAND_BYTES.as_slice();
    while let [n0, n1, x0, x1, y0, y1, rest @ ..] = bytes {
        let count = usize::from(u16::from_le_bytes([*n0, *n1]));
        let mut longitude = i32::from(i16::from_le_bytes([*x0, *x1]));
        let mut latitude = i32::from(i16::from_le_bytes([*y0, *y1]));
        let tenths = |value: i32| f64::from(value) / 10.0;
        visit(tenths(longitude), tenths(latitude), true);
        let steps = rest
            .get(..count.saturating_sub(1).saturating_mul(2))
            .unwrap_or(rest);
        for step in steps.chunks_exact(2) {
            if let [dx, dy] = *step {
                longitude = longitude.saturating_add(i32::from(i8::from_le_bytes([dx])));
                latitude = latitude.saturating_add(i32::from(i8::from_le_bytes([dy])));
                visit(tenths(longitude), tenths(latitude), false);
            }
        }
        bytes = rest.get(steps.len()..).unwrap_or_default();
    }
}

struct Ring {
    points: Vec<[f64; 3]>,
    degrees: Vec<(f64, f64)>,
}

static RINGS: LazyLock<Vec<Ring>> = LazyLock::new(|| {
    let mut rings: Vec<Ring> = Vec::new();
    walk_world_land(|longitude, latitude, starts| {
        if starts || rings.is_empty() {
            rings.push(Ring {
                points: Vec::new(),
                degrees: Vec::new(),
            });
        }
        if let Some(ring) = rings.last_mut() {
            ring.points.push(unit(longitude, latitude));
            ring.degrees.push((longitude, latitude));
        }
    });
    rings
});

impl Ring {
    /// Whether the ring encloses the place. Rings do not cross the antimeridian, so
    /// a planar test in degrees answers it.
    fn encloses(&self, longitude: f64, latitude: f64) -> bool {
        let mut inside = false;
        let mut previous = self.degrees.last().copied();
        for &(x, y) in &self.degrees {
            if let Some((px, py)) = previous
                && (y > latitude) != (py > latitude)
                && longitude < (px - x) * (latitude - y) / (py - y) + x
            {
                inside = !inside;
            }
            previous = Some((x, y));
        }
        inside
    }
}

fn unit(longitude: f64, latitude: f64) -> [f64; 3] {
    let (lambda, phi) = (longitude.to_radians(), latitude.to_radians());
    [
        phi.cos() * lambda.cos(),
        phi.cos() * lambda.sin(),
        phi.sin(),
    ]
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn scaled(a: [f64; 3], by: f64) -> [f64; 3] {
    [a[0] * by, a[1] * by, a[2] * by]
}

fn added(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn normalized(a: [f64; 3]) -> [f64; 3] {
    let length = dot(a, a).sqrt();
    if length < 1e-12 {
        [0.0, 0.0, 1.0]
    } else {
        scaled(a, 1.0 / length)
    }
}

pub fn degrees_apart(a: (f64, f64), b: (f64, f64)) -> f64 {
    dot(unit(a.0, a.1), unit(b.0, b.1))
        .clamp(-1.0, 1.0)
        .acos()
        .to_degrees()
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct View {
    pub longitude: f64,
    pub latitude: f64,
}

const MAX_TILT_DEGREES: u8 = 70;

impl View {
    fn facing(longitude: f64, latitude: f64) -> Self {
        Self {
            longitude: wrap_longitude(longitude),
            latitude: latitude.clamp(-f64::from(MAX_TILT_DEGREES), f64::from(MAX_TILT_DEGREES)),
        }
    }
}

fn wrap_longitude(longitude: f64) -> f64 {
    (longitude + 180.0).rem_euclid(360.0) - 180.0
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Projection {
    centre: Point,
    radius: f64,
    forward: [f64; 3],
    east: [f64; 3],
    north: [f64; 3],
}

impl Projection {
    fn new(centre: Point, radius: f64, view: View) -> Self {
        let (lambda, phi) = (view.longitude.to_radians(), view.latitude.to_radians());
        Self {
            centre,
            radius,
            forward: unit(view.longitude, view.latitude),
            east: [-lambda.sin(), lambda.cos(), 0.0],
            north: [
                -phi.sin() * lambda.cos(),
                -phi.sin() * lambda.sin(),
                phi.cos(),
            ],
        }
    }

    fn faces(&self, p: [f64; 3]) -> bool {
        dot(p, self.forward) > 0.0
    }

    fn project(&self, p: [f64; 3]) -> Point {
        let (x, y) = (dot(p, self.east), dot(p, self.north));
        let sine = x.hypot(y);
        if sine < 1e-9 {
            let away = if self.faces(p) { 0.0 } else { 2.0 };
            return Point::new(self.centre.x + away * self.radius, self.centre.y);
        }
        let reach = if self.faces(p) {
            self.radius * sine
        } else {
            self.radius * (2.0 - sine)
        };
        Point::new(
            self.centre.x + reach * x / sine,
            self.centre.y - reach * y / sine,
        )
    }

    fn place_at(&self, point: Point) -> Option<(f64, f64)> {
        let x = (point.x - self.centre.x) / self.radius;
        let y = -(point.y - self.centre.y) / self.radius;
        let reach = x * x + y * y;
        if reach > 1.0 {
            return None;
        }
        let p = added(
            added(
                scaled(self.forward, (1.0 - reach).sqrt()),
                scaled(self.east, x),
            ),
            scaled(self.north, y),
        );
        Some((
            p[1].atan2(p[0]).to_degrees(),
            p[2].clamp(-1.0, 1.0).asin().to_degrees(),
        ))
    }

    fn antipode(&self) -> (f64, f64) {
        let p = scaled(self.forward, -1.0);
        (
            p[1].atan2(p[0]).to_degrees(),
            p[2].clamp(-1.0, 1.0).asin().to_degrees(),
        )
    }

    fn frame(&self, path: &mut BezPath) {
        let r = self.radius * 2.5;
        let (x, y) = (self.centre.x, self.centre.y);
        path.move_to((x - r, y - r));
        path.line_to((x + r, y - r));
        path.line_to((x + r, y + r));
        path.line_to((x - r, y + r));
        path.close_path();
    }

    fn ring(&self, path: &mut BezPath, points: impl IntoIterator<Item = [f64; 3]>) {
        let mut started = false;
        for p in points {
            let at = self.project(p);
            if started {
                path.line_to(at);
            } else {
                path.move_to(at);
                started = true;
            }
        }
        if started {
            path.close_path();
        }
    }

    fn land(&self) -> BezPath {
        let mut path = BezPath::new();
        let (longitude, latitude) = self.antipode();
        for ring in RINGS.iter() {
            if ring.points.iter().all(|p| !self.faces(*p)) {
                continue;
            }
            if ring.encloses(longitude, latitude) {
                self.frame(&mut path);
            }
            self.ring(&mut path, ring.points.iter().copied());
        }
        path
    }

    /// Where the sun is more than `angle` degrees from overhead at `unix`: the night
    /// side at 90, with the twilight band beyond it.
    fn dark_side(&self, unix: i64, angle: f64) -> BezPath {
        let away = scaled(sun_overhead(unix), -1.0);
        let reach = (180.0 - angle).to_radians();
        let side = normalized(cross(away, [0.0, 0.0, 1.0]));
        let up = cross(away, side);
        let mut path = BezPath::new();
        if dot(scaled(self.forward, -1.0), away) >= reach.cos() {
            self.frame(&mut path);
        }
        self.ring(
            &mut path,
            (0..72).map(|step| {
                let turn = f64::from(step) * 5.0_f64.to_radians();
                added(
                    scaled(away, reach.cos()),
                    scaled(
                        added(scaled(side, turn.cos()), scaled(up, turn.sin())),
                        reach.sin(),
                    ),
                )
            }),
        );
        path
    }

    fn graticule(&self) -> BezPath {
        let mut path = BezPath::new();
        let mut line = |points: &mut dyn Iterator<Item = [f64; 3]>| {
            let mut started = false;
            for p in points {
                let at = self.project(p);
                if started {
                    path.line_to(at);
                } else {
                    path.move_to(at);
                    started = true;
                }
            }
        };
        for longitude in (-180..180).step_by(30) {
            line(
                &mut (-80..=80)
                    .step_by(5)
                    .map(|latitude| unit(f64::from(longitude), f64::from(latitude))),
            );
        }
        for latitude in (-60..=60).step_by(30) {
            line(
                &mut (-180..=180)
                    .step_by(5)
                    .map(|longitude| unit(f64::from(longitude), f64::from(latitude))),
            );
        }
        path
    }
}

/// A count of seconds within a year, which an `f64` holds exactly.
fn seconds(within_year: i64) -> f64 {
    u32::try_from(within_year).map_or(0.0, f64::from)
}

/// The point where the sun stands overhead at `unix`, as a unit vector: its
/// declination from the day of the year, its longitude from the time of day.
/// Accurate to a degree or so, which is a pixel on the globe.
fn sun_overhead(unix: i64) -> [f64; 3] {
    let day = seconds(unix.rem_euclid(365 * 86_400 + 21_600)) / 86_400.0;
    let declination = -23.44 * (std::f64::consts::TAU * (day + 10.0) / 365.25).cos();
    let hours = seconds(unix.rem_euclid(86_400)) / 3600.0;
    unit(wrap_longitude((12.0 - hours) * 15.0), declination)
}

struct GlobePalette {
    land: vello::peniko::Color,
    night: vello::peniko::Color,
    shade: vello::peniko::Color,
}

impl GlobePalette {
    fn of(colors: &ui::tokens::colors::Colors) -> Self {
        if colors.palette.dark {
            Self {
                land: colors.text_primary.multiply_alpha(0.26),
                night: colors.surfaces.base.multiply_alpha(0.5),
                shade: colors.surfaces.base.multiply_alpha(0.55),
            }
        } else {
            Self {
                land: colors.text_primary.multiply_alpha(0.16),
                night: colors.text_primary.multiply_alpha(0.07),
                shade: colors.text_primary.multiply_alpha(0.08),
            }
        }
    }
}

#[derive(Default)]
pub struct GlobeState {
    pub view: Option<View>,
    /// The zone the globe was last turned to face.
    faced: Option<String>,
    cursor: Option<usize>,
    land: Option<(Projection, BezPath)>,
}

pub type SharedGlobe = Rc<RefCell<GlobeState>>;

/// The globe widget; what it remembers is in its [`GlobeState`].
pub struct TimeZoneGlobe {
    points: Rc<[TimeZonePoint]>,
    selected: String,
    state: SharedGlobe,
    now: i64,
    hover: Option<usize>,
    last_cursor: Point,
    press: Option<(Point, View, bool)>,
    focused: bool,
    hit_radius: f64,
    action: Option<ui::widget::ActionEmitter<String>>,
    rect: Rect,
    tooltip: Option<String>,
}

const DIAMETER: f32 = ui::tokens::Metrics::BASE.figure_size;
const DRAG_SLOP: f64 = ui::tokens::Metrics::BASE.click_slop as f64;

impl TimeZoneGlobe {
    pub fn new(
        points: Rc<[TimeZonePoint]>,
        selected: impl Into<String>,
        state: SharedGlobe,
        now: i64,
    ) -> Self {
        let selected = selected.into();
        {
            let mut shared = state.borrow_mut();
            // Faces the chosen zone's city when it first shows and whenever the zone is
            // chosen elsewhere; a turn the person made by dragging stays until then.
            if (shared.view.is_none() || shared.faced.as_deref() != Some(selected.as_str()))
                && let Some(point) = points.iter().find(|point| point.id == selected)
            {
                let facing = (point.longitude(), point.latitude() * 0.6);
                let in_view = shared.view.is_some_and(|view| {
                    degrees_apart((view.longitude, view.latitude), facing) <= 55.0
                });
                if !in_view {
                    shared.view = Some(View::facing(facing.0, facing.1));
                }
                shared.faced = Some(selected.clone());
            }
        }
        Self {
            points,
            selected,
            state,
            now,
            hover: None,
            last_cursor: Point::ZERO,
            press: None,
            focused: false,
            hit_radius: f64::from(ui::tokens::Metrics::BASE.min_pointer_target) / 2.0,
            action: None,
            rect: Rect::ZERO,
            tooltip: None,
        }
    }

    #[must_use]
    pub fn on_action(mut self, binding: Option<tilo_actions::ActionBinding<String>>) -> Self {
        self.action = binding.map(ui::widget::ActionEmitter::new);
        self
    }

    fn view(&self) -> View {
        self.state
            .borrow()
            .view
            .unwrap_or_else(|| View::facing(10.0, 30.0))
    }

    fn projection(&self) -> Projection {
        Projection::new(self.rect.center(), self.radius(), self.view())
    }

    fn radius(&self) -> f64 {
        (self.rect.width().min(self.rect.height()) / 2.0 - 1.0).max(1.0)
    }

    fn unit_of(&self, index: usize) -> Option<[f64; 3]> {
        self.points
            .get(index)
            .map(|point| unit(point.longitude(), point.latitude()))
    }

    fn point_at(&self, position: Point) -> Option<usize> {
        let projection = self.projection();
        if position.distance(projection.centre) > projection.radius + self.hit_radius {
            return None;
        }
        (0..self.points.len())
            .filter_map(|index| {
                let p = self.unit_of(index)?;
                projection.faces(p).then(|| {
                    let at = projection.project(p);
                    (index, at.distance(position))
                })
            })
            .filter(|(_, distance)| *distance <= self.hit_radius)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(index, _)| index)
    }

    fn selected_index(&self) -> Option<usize> {
        self.points
            .iter()
            .position(|point| point.id == self.selected)
    }

    fn cursor(&self) -> Option<usize> {
        self.state.borrow().cursor
    }

    fn move_cursor(&self, east: f64, north: f64) {
        let current = self
            .cursor()
            .or_else(|| self.selected_index())
            .unwrap_or_default();
        let Some(origin) = self.points.get(current) else {
            return;
        };
        let target = self
            .points
            .iter()
            .enumerate()
            .filter(|(index, _)| *index != current)
            .filter_map(|(index, candidate)| {
                let dx = wrap_longitude(candidate.longitude() - origin.longitude());
                let dy = candidate.latitude() - origin.latitude();
                let along = dx * east + dy * north;
                if along <= 0.0 {
                    return None;
                }
                let across = (dx * north - dy * east).abs();
                Some((index, along + across * 2.5))
            })
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(index, _)| index);
        if let Some(target) = target
            && let Some(point) = self.points.get(target)
        {
            let mut state = self.state.borrow_mut();
            state.cursor = Some(target);
            let view = state.view.unwrap_or_else(|| View::facing(0.0, 0.0));
            let facing = degrees_apart(
                (view.longitude, view.latitude),
                (point.longitude(), point.latitude()),
            );
            if facing > 55.0 {
                state.view = Some(View::facing(point.longitude(), point.latitude()));
            }
        }
    }

    fn commit(&mut self, index: usize) {
        if let Some(point) = self.points.get(index) {
            self.state.borrow_mut().cursor = Some(index);
            if self.selected == point.id {
                return;
            }
            self.selected.clone_from(&point.id);
            self.state.borrow_mut().faced = Some(point.id.clone());
            if let Some(action) = &mut self.action {
                let _ = action.emit(point.id.clone());
            }
        }
    }

    fn nearest_to(&self, place: (f64, f64)) -> Option<usize> {
        self.points
            .iter()
            .enumerate()
            .map(|(index, point)| {
                (
                    index,
                    degrees_apart(place, (point.longitude(), point.latitude())),
                )
            })
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(index, _)| index)
    }

    fn drag_to(&mut self, position: Point) -> bool {
        let Some((start, view, dragging)) = self.press else {
            return false;
        };
        if !dragging && position.distance(start) < DRAG_SLOP {
            return false;
        }
        let radius = self.projection().radius;
        let longitude = view.longitude - ((position.x - start.x) / radius).to_degrees();
        let latitude = view.latitude + ((position.y - start.y) / radius).to_degrees();
        self.state.borrow_mut().view = Some(View::facing(longitude, latitude));
        self.press = Some((start, view, true));
        true
    }

    fn label_of(&self, index: usize) -> Option<String> {
        self.points.get(index).map(|point| {
            t_args(
                "time-map-place",
                &[
                    ("city", ArgValue::from(point.city.as_str())),
                    ("region", ArgValue::from(point.region.as_str())),
                ],
            )
        })
    }
    fn paint_cities(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        projection: &Projection,
    ) {
        if self.points.is_empty() {
            return;
        }
        let colors = &theme.tokens.colors;

        let selected = self.selected_index();
        let cursor = self.cursor().filter(|_| self.focused);
        let dots = colors.text_primary.multiply_alpha(0.55);
        let mut marks = BezPath::new();
        for index in 0..self.points.len() {
            if Some(index) == selected {
                continue;
            }
            let Some(p) = self.unit_of(index) else {
                continue;
            };
            if projection.faces(p) {
                let at = projection.project(p);
                marks.extend(Circle::new(at, 1.8).path_elements(0.1));
            }
        }
        scene.fill(Fill::NonZero, transform, dots, None, &marks);
        for (index, emphasis) in [(self.hover, false), (cursor, false), (selected, true)] {
            let Some(index) = index else { continue };
            let Some(p) = self.unit_of(index) else {
                continue;
            };
            if !projection.faces(p) {
                continue;
            }
            let at = projection.project(p);
            if emphasis {
                // The chosen city, ringed in the ground so it stands off the land and the night.
                // ratchet-allow(raw_paint): the time-zone globe is content drawing (land, night and shade on the sphere)
                scene.fill(
                    Fill::NonZero,
                    transform,
                    colors.surfaces.content,
                    None,
                    &Circle::new(at, 6.5),
                );
                scene.fill(
                    Fill::NonZero,
                    transform,
                    colors.intent.accent,
                    None,
                    &Circle::new(at, 4.5),
                );
            } else {
                scene.stroke(
                    &Stroke::new(2.0),
                    transform,
                    if Some(index) == cursor {
                        theme.border_focus
                    } else {
                        colors.text_primary
                    },
                    None,
                    &Circle::new(at, 6.0),
                );
            }
        }
        if self.focused {
            scene.stroke(
                &Stroke::new(2.0),
                transform,
                theme.border_focus,
                None,
                &Circle::new(projection.centre, projection.radius + 3.0),
            );
        }
    }
}

impl Widget for TimeZoneGlobe {
    ui::provides!(dyn ui::widget::controls::ActionControls);

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.hit_radius = f64::from(theme.tokens.metrics.min_pointer_target) / 2.0;
        let diameter = DIAMETER * theme.tokens.metrics.min_pointer_target
            / ui::tokens::Metrics::BASE.min_pointer_target;
        // As wide as the column allows, never wider than its diameter, and always round.
        let style = Style {
            size: Size {
                width: Dimension::length(diameter),
                height: Dimension::auto(),
            },
            max_size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::auto(),
            },
            min_size: Size {
                width: Dimension::length(diameter / 2.0),
                height: Dimension::auto(),
            },
            aspect_ratio: Some(1.0),
            flex_shrink: 1.0,
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    ui::rect_geometry!();

    #[allow(
        clippy::cast_possible_truncation,
        reason = "the shade's radius is pixels, which an f32 holds"
    )]
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        _text_renderer: &mut ui::text_render::TextRenderer,
        theme: &Theme,
        _presence: &ui::presence::PresenceCtx,
    ) {
        ui::outside_paint::note();
        let colors = &theme.tokens.colors;
        let palette = GlobePalette::of(colors);
        // Paths are built around the origin and moved into place, so scrolling the page
        // never projects the land again.
        let transform = transform.pre_translate(self.rect.center().to_vec2());
        let projection = Projection::new(Point::ZERO, self.radius(), self.view());
        let disk = Circle::new(projection.centre, projection.radius);
        scene.fill(
            Fill::NonZero,
            transform,
            colors.surfaces.raised,
            None,
            &disk,
        );

        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, &disk);
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            colors.overlay_border_subtle,
            None,
            &projection.graticule(),
        );
        {
            let mut state = self.state.borrow_mut();
            let fresh = state
                .land
                .as_ref()
                .is_some_and(|(cached, _)| *cached == projection);
            if !fresh {
                state.land = Some((projection, projection.land()));
            }
            if let Some((_, land)) = &state.land {
                scene.fill(Fill::EvenOdd, transform, palette.land, None, land);
            }
        }
        let night = palette.night;
        for angle in [96.0, 90.0] {
            scene.fill(
                Fill::EvenOdd,
                transform,
                night,
                None,
                &projection.dark_side(self.now, angle),
            );
        }
        let shade = vello::peniko::Gradient::new_radial(
            Point::new(-projection.radius * 0.35, -projection.radius * 0.4),
            (projection.radius * 1.55) as f32,
        )
        .with_stops([
            (0.0_f32, palette.shade.multiply_alpha(0.0)),
            (0.6_f32, palette.shade.multiply_alpha(0.0)),
            (1.0_f32, palette.shade),
        ]);
        scene.fill(
            Fill::NonZero,
            transform,
            &vello::peniko::Brush::Gradient(shade),
            None,
            &disk,
        );
        scene.pop_layer();
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            colors.overlay_border_strong,
            None,
            &disk,
        );

        self.paint_cities(scene, transform, theme, &projection);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor = ctx.logical(position);
                if self.drag_to(self.last_cursor) {
                    self.hover = None;
                    *ctx.handled = true;
                    return;
                }
                let hover = self.point_at(self.last_cursor);
                if hover != self.hover {
                    self.hover = hover;
                    self.tooltip = hover.and_then(|index| self.label_of(index));
                    *ctx.handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let projection = self.projection();
                if self.last_cursor.distance(projection.centre) <= projection.radius {
                    self.press = Some((self.last_cursor, self.view(), false));
                    *ctx.focus_target = Some(ctx.node_id);
                    *ctx.handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                let Some((_, _, dragged)) = self.press.take() else {
                    return;
                };
                *ctx.handled = true;
                if dragged {
                    return;
                }
                if let Some(index) = self
                    .point_at(self.last_cursor)
                    .or_else(|| self.nearest_to(self.projection().place_at(self.last_cursor)?))
                {
                    self.commit(index);
                }
            }
            WindowEvent::KeyboardInput { event, .. }
                if self.focused && event.state == ElementState::Pressed =>
            {
                use winit::keyboard::{Key, NamedKey};
                match event.logical_key {
                    Key::Named(NamedKey::ArrowLeft) => self.move_cursor(-1.0, 0.0),
                    Key::Named(NamedKey::ArrowRight) => self.move_cursor(1.0, 0.0),
                    Key::Named(NamedKey::ArrowUp) => self.move_cursor(0.0, 1.0),
                    Key::Named(NamedKey::ArrowDown) => self.move_cursor(0.0, -1.0),
                    Key::Named(NamedKey::Enter | NamedKey::Space) => {
                        if let Some(index) = self.cursor() {
                            self.commit(index);
                        }
                    }
                    _ => return,
                }
                *ctx.handled = true;
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "TimeZoneGlobe"
    }

    fn test_id(&self) -> Option<&str> {
        Some("time.timezone-map")
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        if self.press.is_some_and(|(_, _, dragging)| dragging) {
            return Some(winit::window::CursorIcon::Grabbing);
        }
        if self.point_at(point).is_some() {
            return Some(winit::window::CursorIcon::Pointer);
        }
        let projection = self.projection();
        (point.distance(projection.centre) <= projection.radius)
            .then_some(winit::window::CursorIcon::Grab)
    }

    fn tooltip(&self) -> Option<std::borrow::Cow<'_, str>> {
        self.tooltip.as_deref().map(std::borrow::Cow::Borrowed)
    }

    fn is_focusable(&self) -> bool {
        !self.points.is_empty()
    }

    fn set_focus(&mut self, focused: bool) {
        self.focused = focused;
        if focused && self.cursor().is_none() {
            let start = self.selected_index().or(Some(0));
            self.state.borrow_mut().cursor = start;
        }
    }

    fn is_focused(&self) -> bool {
        self.focused
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};
        Some(
            AccessNode::new(Role::Canvas)
                .with_name(t("time-map-a11y-name"))
                .with_description(t("time-map-a11y-desc"))
                .with_value(
                    self.cursor()
                        .filter(|_| self.focused)
                        .and_then(|index| self.points.get(index))
                        .map_or_else(|| self.selected.clone(), |point| point.id.clone()),
                )
                .with_state(
                    AccessState::new()
                        .focusable(self.is_focusable())
                        .focused(self.focused),
                ),
        )
    }

    fn perform_action(
        &mut self,
        action: &ui::a11y::Action,
        _key_path: &[u64],
        _state: &mut ui::widget::Edits<'_>,
        _root: &ModelPath,
    ) -> bool {
        match action {
            ui::a11y::Action::Activate => {
                if let Some(index) = self.cursor().or_else(|| self.selected_index()) {
                    self.commit(index);
                    true
                } else {
                    false
                }
            }
            ui::a11y::Action::SetValue(value) => {
                if let Some(index) = self.points.iter().position(|point| &point.id == value) {
                    self.commit(index);
                    true
                } else {
                    false
                }
            }
            _ => false,
        }
    }
}

impl ui::widget::controls::ActionControls for TimeZoneGlobe {
    ui::widget::controls::optional_emitter_controls!();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn projection(longitude: f64, latitude: f64) -> Projection {
        Projection::new(
            Point::new(100.0, 100.0),
            100.0,
            View::facing(longitude, latitude),
        )
    }

    #[test]
    fn the_packed_land_decodes_to_rings_on_the_globe() {
        let (mut rings, mut points, mut northmost) = (0, 0, f64::MIN);
        walk_world_land(|longitude, latitude, starts| {
            rings += usize::from(starts);
            points += 1;
            assert!((-180.0..=180.0).contains(&longitude), "{longitude}");
            assert!((-90.0..=90.0).contains(&latitude), "{latitude}");
            northmost = northmost.max(latitude);
        });
        assert_eq!(rings, 119);
        assert_eq!(points, 4_412);
        assert!(northmost > 83.0, "Greenland's north coast is there");
        assert_eq!(RINGS.len(), 119);
    }

    #[test]
    fn a_place_is_drawn_where_a_click_finds_it() {
        let globe = projection(10.0, 45.0);
        let centre = globe.project(unit(10.0, 45.0));
        assert!(centre.distance(Point::new(100.0, 100.0)) < 1e-6);
        for place in [(16.37, 48.21), (-3.7, 40.4), (30.0, 60.0)] {
            let at = globe.project(unit(place.0, place.1));
            let back = globe.place_at(at).unwrap();
            assert!(
                degrees_apart(place, back) < 1e-6,
                "{place:?} came back {back:?}"
            );
        }
        assert!(globe.project(unit(20.0, 45.0)).x > 100.0);
        assert!(globe.project(unit(10.0, 55.0)).y < 100.0);
        let sydney = globe.project(unit(151.2, -33.9));
        assert!(!globe.faces(unit(151.2, -33.9)));
        assert!(sydney.distance(Point::new(100.0, 100.0)) > 100.0);
        assert!(globe.place_at(Point::new(250.0, 100.0)).is_none());
    }

    #[test]
    fn the_land_is_under_the_cities_from_every_side() {
        let inland = [(13.4, 52.5), (-104.9, 39.7), (116.4, 39.9), (-47.9, -15.8)];
        let at_sea = [(-30.0, 30.0), (-150.0, 0.0), (80.0, -30.0)];
        for (longitude, latitude) in [(0.0, 0.0), (10.0, 45.0), (-100.0, -45.0), (120.0, 20.0)] {
            let globe = projection(longitude, latitude);
            let land = globe.land();
            let filled =
                |place: (f64, f64)| land.winding(globe.project(unit(place.0, place.1))) % 2 != 0;
            for place in inland {
                if globe.faces(unit(place.0, place.1)) {
                    assert!(
                        filled(place),
                        "{place:?} is land seen from {longitude},{latitude}"
                    );
                }
            }
            for place in at_sea {
                if globe.faces(unit(place.0, place.1)) {
                    assert!(
                        !filled(place),
                        "{place:?} is sea seen from {longitude},{latitude}"
                    );
                }
            }
        }
    }

    #[test]
    fn the_night_is_where_the_sun_is_down() {
        // 2026-03-20 12:00 UTC.
        let noon = 1_774_008_000;
        for (longitude, latitude) in [(0.0, 0.0), (180.0, 0.0), (90.0, 30.0)] {
            let globe = projection(longitude, latitude);
            let night = globe.dark_side(noon, 90.0);
            let dark =
                |place: (f64, f64)| night.winding(globe.project(unit(place.0, place.1))) % 2 != 0;
            if globe.faces(unit(0.0, 0.0)) {
                assert!(
                    !dark((0.0, 0.0)),
                    "Greenwich at noon, seen from {longitude}"
                );
            }
            if globe.faces(unit(179.0, 0.0)) {
                assert!(
                    dark((179.0, 0.0)),
                    "the date line at midnight, seen from {longitude}"
                );
            }
        }
    }

    #[allow(
        clippy::cast_possible_truncation,
        reason = "fixture coordinates are within a few hundred degrees"
    )]
    fn city(id: &str, longitude: f64, latitude: f64) -> TimeZonePoint {
        let (region, city) = id.split_once('/').unwrap();
        TimeZonePoint {
            id: id.to_owned(),
            region: region.to_owned(),
            city: city.to_owned(),
            latitude_e6: (latitude * 1e6) as i32,
            longitude_e6: (longitude * 1e6) as i32,
        }
    }

    #[test]
    fn the_globe_selects_the_nearest_zone_and_turns_to_the_one_the_picker_chose() {
        let points: Rc<[TimeZonePoint]> = vec![
            city("Europe/Amsterdam", 4.9, 52.37),
            city("America/New_York", -74.0, 40.71),
            city("Asia/Tokyo", 139.69, 35.69),
        ]
        .into();
        let state = SharedGlobe::default();
        let globe =
            TimeZoneGlobe::new(Rc::clone(&points), "Europe/Amsterdam", Rc::clone(&state), 0);
        let facing = |state: &SharedGlobe| {
            let view = state.borrow().view.unwrap();
            (view.longitude, view.latitude)
        };
        assert!(degrees_apart(facing(&state), (4.9, 52.37)) < 40.0);
        assert_eq!(globe.nearest_to((5.12, 52.09)), Some(0));
        assert_eq!(globe.nearest_to((137.0, 36.0)), Some(2));

        state.borrow_mut().view = Some(View::facing(10.0, 40.0));
        let _ = TimeZoneGlobe::new(Rc::clone(&points), "Europe/Amsterdam", Rc::clone(&state), 0);
        assert_eq!(facing(&state), (10.0, 40.0));

        let _ = TimeZoneGlobe::new(Rc::clone(&points), "Asia/Tokyo", Rc::clone(&state), 0);
        assert!(degrees_apart(facing(&state), (139.69, 35.69)) < 40.0);
    }
}
