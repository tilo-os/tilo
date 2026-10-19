//! The time-zone globe: the Earth drawn as a globe with the land, the night
//! side and every zone's city, turned by dragging. Clicking a place lists the
//! cities near it (the pane's nearby list); clicking a city's dot chooses its
//! zone. The arrow keys walk from city to city and Enter chooses one.
//!
//! The land is Natural Earth's 1:110m polygons (`data/world-land.b64`), the
//! cities are the installed tzdata's, and the night side is where the sun
//! is down at the clock's current time.
//!
//! **Projection.** The front hemisphere is orthographic. Points behind it
//! continue outward along the same azimuth (radius `2R − R·sin c`), so every
//! coastline stays one closed, non-crossing ring however the globe is turned,
//! and clipping the fill to the disk leaves exactly the visible land. A ring
//! that encloses the point opposite the view's centre would map to the
//! outside of its image, so the land is filled even-odd with a frame around
//! it.

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

use super::TimeZonePoint;
use crate::i18n::{t, t_args};
use ui::i18n::ArgValue;

/// The land masses, from Natural Earth's 1:110m land polygons
/// (`data/world-land.b64`, format and provenance in `data/README.md`).
/// tzdata publishes where each zone's city is, but nothing installed
/// describes the coastlines the cities sit on.
const WORLD_LAND: &str = include_str!("../../../data/world-land.b64");

/// The value of base64 digit `c`, or `None` for padding and line breaks.
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

/// How many bytes standard, padded base64 `text` holds.
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

/// The `N` bytes of standard, padded base64 `text`; line breaks are
/// skipped. Evaluated while compiling, so the app carries the bytes, not
/// their base64 text and a decoder.
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

/// [`WORLD_LAND`]'s bytes.
static WORLD_LAND_BYTES: [u8; decoded_len(WORLD_LAND.as_bytes())] =
    decode_base64(WORLD_LAND.as_bytes());

/// Walk [`WORLD_LAND`]: each ring is a little-endian `u16` point count, its
/// first point as two `i16` (longitude, latitude) in tenths of a degree,
/// then one `i8` pair per further point, the step from the point before.
/// `visit` gets each point in degrees and whether it starts a ring.
pub(super) fn walk_world_land(mut visit: impl FnMut(f64, f64, bool)) {
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

/// One coastline: its points as unit vectors, and its points in degrees for
/// the point-in-ring test.
struct Ring {
    points: Vec<[f64; 3]>,
    degrees: Vec<(f64, f64)>,
}

/// The coastlines, decoded once.
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
    /// Whether the ring encloses the place at `longitude`, `latitude`:
    /// the rings do not cross the antimeridian, so a planar test in degrees
    /// answers it.
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

/// The unit vector of a place, from degrees.
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

/// The great-circle distance between two places in degrees.
pub(super) fn degrees_apart(a: (f64, f64), b: (f64, f64)) -> f64 {
    dot(unit(a.0, a.1), unit(b.0, b.1))
        .clamp(-1.0, 1.0)
        .acos()
        .to_degrees()
}

/// Which way the globe faces: the place at its centre, in degrees.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct View {
    pub longitude: f64,
    pub latitude: f64,
}

/// How far the globe may tilt toward a pole: past this the land near the
/// pole is all that shows.
const MAX_TILT: f64 = 70.0;

impl View {
    fn facing(longitude: f64, latitude: f64) -> Self {
        Self {
            longitude: wrap_longitude(longitude),
            latitude: latitude.clamp(-MAX_TILT, MAX_TILT),
        }
    }
}

fn wrap_longitude(longitude: f64) -> f64 {
    (longitude + 180.0).rem_euclid(360.0) - 180.0
}

/// The globe's projection at a size and a view.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Projection {
    centre: Point,
    radius: f64,
    /// The view's centre, east and north as unit vectors.
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

    /// Whether `p` faces the viewer.
    fn faces(&self, p: [f64; 3]) -> bool {
        dot(p, self.forward) > 0.0
    }

    /// Where `p` lands: orthographic in front, continued outward behind
    /// (see the module's note).
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

    /// The place under `point`, when it is on the globe.
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

    /// The opposite of the view's centre, in degrees.
    fn antipode(&self) -> (f64, f64) {
        let p = scaled(self.forward, -1.0);
        (
            p[1].atan2(p[0]).to_degrees(),
            p[2].clamp(-1.0, 1.0).asin().to_degrees(),
        )
    }

    /// A frame around everything the projection reaches, for a ring that
    /// encloses the antipode (filled even-odd, it turns the ring inside out).
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

    /// The land, filled even-odd.
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

    /// Where the sun is more than `angle` degrees from overhead, at `unix`:
    /// the night side at 90, with the twilight band beyond it.
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

/// The point where the sun stands overhead at `unix`, as a unit vector: its
/// declination from the day of the year, its longitude from the time of day.
/// Accurate to a degree or so, which is a pixel on the globe.
fn sun_overhead(unix: i64) -> [f64; 3] {
    let day = unix.rem_euclid(365 * 86_400 + 21_600) as f64 / 86_400.0;
    let declination = -23.44 * (std::f64::consts::TAU * (day + 10.0) / 365.25).cos();
    let hours = unix.rem_euclid(86_400) as f64 / 3600.0;
    unit(wrap_longitude((12.0 - hours) * 15.0), declination)
}

/// The globe's colours, from the theme: land a step off the sea in the
/// theme's own ink, and night and the sphere's falloff darker in both
/// polarities (dark themes darken toward their ground, light ones with a
/// wash of their ink).
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

/// What the globe keeps across the pane's rebuilds: which way it faces,
/// where the nearby list is centred, the keyboard's city, and the land
/// already projected for the current size and view.
#[derive(Default)]
pub(super) struct GlobeState {
    pub view: Option<View>,
    /// The place the nearby list is around, when the person picked one.
    pub focus: Option<(f64, f64)>,
    /// Counts focus changes, so the pane rebuilds its list once per change.
    pub focus_revision: u64,
    cursor: Option<usize>,
    land: Option<(Projection, BezPath)>,
}

pub(super) type SharedGlobe = Rc<RefCell<GlobeState>>;

/// The globe widget. Rebuilt with the pane; everything it must remember is
/// in its [`GlobeState`].
pub(super) struct TimeZoneGlobe {
    points: Rc<[TimeZonePoint]>,
    selected: String,
    state: SharedGlobe,
    /// The focus revision this widget was built at.
    built_revision: u64,
    /// The clock's time, for the night side.
    now: i64,
    hover: Option<usize>,
    last_cursor: Point,
    /// Where a press landed and the view it started from, while the button
    /// is down; and whether it has moved far enough to be a drag.
    press: Option<(Point, View, bool)>,
    focused: bool,
    hit_radius: f64,
    action: Option<ui::widget::ActionEmitter<String>>,
    rect: Rect,
    tooltip: Option<String>,
}

/// The globe's diameter, at the default text size.
const DIAMETER: f32 = 280.0;
/// How far a press may move and still be a click.
const DRAG_SLOP: f64 = 4.0;

impl TimeZoneGlobe {
    pub(super) fn new(
        points: Rc<[TimeZonePoint]>,
        selected: impl Into<String>,
        state: SharedGlobe,
        now: i64,
    ) -> Self {
        let selected = selected.into();
        let built_revision = {
            let mut shared = state.borrow_mut();
            if shared.view.is_none()
                && let Some(point) = points.iter().find(|point| point.id == selected)
            {
                shared.view = Some(View::facing(point.longitude(), point.latitude() * 0.6));
            }
            shared.focus_revision
        };
        Self {
            points,
            selected,
            state,
            built_revision,
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

    pub(super) fn on_action(
        mut self,
        binding: Option<tilo_actions::ActionBinding<String>>,
    ) -> Self {
        self.action = binding.map(ui::widget::ActionEmitter::new);
        self
    }

    fn view(&self) -> View {
        self.state.borrow().view.unwrap_or(View::facing(10.0, 30.0))
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

    /// The city whose dot is under `position`, on the front of the globe.
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

    /// Move the keyboard's city to the nearest one in a direction, and turn
    /// the globe to it.
    fn move_cursor(&mut self, east: f64, north: f64) {
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
            let view = state.view.unwrap_or(View::facing(0.0, 0.0));
            // Turn only as far as keeps the city in view.
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
            self.focus_on((point.longitude(), point.latitude()));
            if self.selected == point.id {
                return;
            }
            self.selected.clone_from(&point.id);
            if let Some(action) = &mut self.action {
                let _ = action.emit(point.id.clone());
            }
        }
    }

    /// Centre the nearby list on a place.
    fn focus_on(&self, place: (f64, f64)) {
        let mut state = self.state.borrow_mut();
        state.focus = Some(place);
        state.focus_revision += 1;
    }

    fn drag_to(&mut self, position: Point) -> bool {
        let Some((start, view, dragging)) = self.press else {
            return false;
        };
        if !dragging && position.distance(start) < DRAG_SLOP {
            return false;
        }
        let radius = self.projection().radius;
        // A drag across the globe's radius turns it by about a radian.
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
        // As wide as it may be up to its diameter, never wider than the
        // column it is in, and always round.
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

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        _text_renderer: &mut ui::text_render::TextRenderer,
        theme: &Theme,
        _presence: &ui::presence::PresenceCtx,
    ) {
        let colors = &theme.tokens.colors;
        let palette = GlobePalette::of(colors);
        // The globe's paths are built around the origin and moved into
        // place, so scrolling the page never projects the land again.
        let transform = transform * Affine::translate(self.rect.center().to_vec2());
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
        // The night: the twilight band, then the night inside it.
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
        // Light from the upper left: the far edge of the sphere falls off.
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

        // The pane shows no globe without zones to place on it.
        if self.points.is_empty() {
            return;
        }

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
                // The chosen city: the person's accent, ringed in the ground
                // so it stands off the land and the night.
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
                if let Some(index) = self.point_at(self.last_cursor) {
                    self.commit(index);
                } else if let Some(place) = self.projection().place_at(self.last_cursor) {
                    self.focus_on(place);
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

    fn take_structure_dirty(&mut self) -> bool {
        // A new focus changes the nearby list beside the globe.
        self.state.borrow().focus_revision != self.built_revision
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

    fn tooltip(&self) -> Option<&str> {
        self.tooltip.as_deref()
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
    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        self.action
            .as_mut()
            .and_then(ui::widget::ActionEmitter::take)
    }
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

    /// The packed land data decodes whole: every ring, every point on the
    /// globe.
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

    /// The front of the globe is where its centre faces, a click finds the
    /// place it was drawn at, and the back continues outside the disk.
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
        // East is to the right and north is up.
        assert!(globe.project(unit(20.0, 45.0)).x > 100.0);
        assert!(globe.project(unit(10.0, 55.0)).y < 100.0);
        // Sydney is behind the globe, outside the disk.
        let sydney = globe.project(unit(151.2, -33.9));
        assert!(!globe.faces(unit(151.2, -33.9)));
        assert!(sydney.distance(Point::new(100.0, 100.0)) > 100.0);
        assert!(globe.place_at(Point::new(250.0, 100.0)).is_none());
    }

    /// Turned to any side, the globe draws the land under its cities and
    /// open water elsewhere, including views whose far side lies inside a
    /// continent (the even-odd frame).
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

    /// The night side is opposite the sun: at noon UTC near an equinox,
    /// Greenwich is in daylight and the date line in the night.
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
}
