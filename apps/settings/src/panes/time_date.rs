//! Time & Date pane — clock, time zone, and date/time display format.
//!
//! The clock-format control here is a *local* view onto the same locale the
//! Region & Formats pane owns: the 24-hour clock is `locale.time_format` of
//! the person's settings, one stored value surfaced in both panes. Automatic
//! time and the time zone are this device's, applied through timedated.
//!
//! ## Live vs. sample
//!
//! - **Current time / date** are read from the device's clock ([`Clock`]),
//!   formatted per the 24-hour choice; a capture shows a fixed time.
//! - **Time zone** is read from timedated, falling back to `/etc/timezone`
//!   (or the `/etc/localtime` symlink target) only when the service does not
//!   report it. Choices come from `timedatectl list-timezones`; map coordinates
//!   are joined from the installed tzdata tables, never an embedded list.
//! - **Set time automatically** and the time zone are [`TimeDateEffect`]
//!   jobs through `timedatectl`; the setter needs privilege, so a denial is
//!   the effect's failure, shown on the pane.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};
use ui::widget::TaffyTreeExt;

use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use ui::LocalState;
use ui::framework::{EffectAnswer, EffectCx, Effected};
use ui::theme::Theme;
use ui::tilo_resource;
use ui::widget::{AppModel, ModelPath, Toggle, Widget};
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Circle, Point, Rect, RoundedRect, Stroke};
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::device_read::Reading;
use crate::effects::effect;
use crate::host::{Followed, PaneCx};
use crate::i18n::{t, t_args};
use crate::resource::Settings;
use crate::settings_app::SettingsApp;
use crate::widgets::{Item, block, card, error_row, note, rows, setting_row, value_row};
use tilo_locale::Locale;
use ui::clock::TimeStyle;
use ui::i18n::ArgValue;
use ui::lock::MutexRecover as _;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TimeZonePoint {
    pub id: String,
    pub region: String,
    pub city: String,
    /// Published by the installed tzdata `zone1970.tab`/`zone.tab`, not
    /// guessed from the identifier. Stored as microdegrees so the model stays
    /// `Eq` and exact enough for a compact picker.
    pub latitude_e6: i32,
    pub longitude_e6: i32,
}

impl TimeZonePoint {
    fn latitude(&self) -> f64 {
        f64::from(self.latitude_e6) / 1_000_000.0
    }

    fn longitude(&self) -> f64 {
        f64::from(self.longitude_e6) / 1_000_000.0
    }
}

/// A map-friendly model over the exact IANA ids accepted by timedated.
/// Coordinates come from the same installed tzdata package; this deliberately
/// has no embedded city/coordinate database. Aliases absent from the tzdata
/// coordinate table remain available in the searchable selector.
pub fn timezone_map(zones: &[String]) -> Vec<TimeZonePoint> {
    const TABLES: &[&str] = &[
        "/usr/share/zoneinfo/zone1970.tab",
        "/usr/share/zoneinfo/zone.tab",
    ];
    // `zone1970.tab` lists one zone per region of identical clocks since
    // 1970; zones merged into another (Europe/Amsterdam into Brussels) are
    // only in `zone.tab`. Reading both, the newer first, puts every zone the
    // device accepts on the map, including the one it may be set to.
    let tables: String = TABLES
        .iter()
        .filter_map(|path| fs::read_to_string(path).ok())
        .collect::<Vec<_>>()
        .join("\n");
    timezone_map_from_tab(zones, &tables)
}

fn timezone_map_from_tab(zones: &[String], table: &str) -> Vec<TimeZonePoint> {
    let accepted: HashSet<&str> = zones.iter().map(String::as_str).collect();
    let mut seen = HashSet::new();
    let mut points: Vec<_> = table
        .lines()
        .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
        .filter_map(|line| {
            let mut columns = line.split('\t');
            let _countries = columns.next()?;
            let coordinate = columns.next()?;
            let id = columns.next()?;
            if !accepted.contains(id) || !seen.insert(id) {
                return None;
            }
            let (latitude_e6, longitude_e6) = parse_iso6709_coordinate(coordinate)?;
            let (region, city) = id.split_once('/')?;
            Some(TimeZonePoint {
                id: id.to_string(),
                region: region.replace('_', " "),
                city: city.replace('_', " ").replace('/', ", "),
                latitude_e6,
                longitude_e6,
            })
        })
        .collect();
    // Stable geographic order makes keyboard movement and screenshots
    // independent of the table's packaging order.
    points.sort_by_key(|point| (point.longitude_e6, -point.latitude_e6, point.id.clone()));
    points
}

/// Parse the compact ISO 6709 latitude+longitude form used by tzdata:
/// `+DDMM+DDDMM` or `+DDMMSS+DDDMMSS` (and negative equivalents).
fn parse_iso6709_coordinate(value: &str) -> Option<(i32, i32)> {
    let split = value
        .char_indices()
        .skip(1)
        .find_map(|(i, c)| matches!(c, '+' | '-').then_some(i))?;
    let latitude = parse_coordinate_part(value.get(..split)?, true)?;
    let longitude = parse_coordinate_part(value.get(split..)?, false)?;
    Some((
        (latitude * 1_000_000.0).round() as i32,
        (longitude * 1_000_000.0).round() as i32,
    ))
}

fn parse_coordinate_part(value: &str, latitude: bool) -> Option<f64> {
    let sign = match value.as_bytes().first().copied()? {
        b'+' => 1.0,
        b'-' => -1.0,
        _ => return None,
    };
    let digits = value.get(1..)?;
    let degree_digits = if latitude { 2 } else { 3 };
    if digits.len() != degree_digits + 2 && digits.len() != degree_digits + 4 {
        return None;
    }
    let degrees: f64 = digits.get(..degree_digits)?.parse().ok()?;
    let minutes: f64 = digits.get(degree_digits..degree_digits + 2)?.parse().ok()?;
    let seconds: f64 = if digits.len() == degree_digits + 4 {
        digits.get(degree_digits + 2..)?.parse().ok()?
    } else {
        0.0
    };
    let limit = if latitude { 90.0 } else { 180.0 };
    if degrees > limit || minutes >= 60.0 || seconds >= 60.0 {
        return None;
    }
    let coordinate = degrees + minutes / 60.0 + seconds / 3600.0;
    (coordinate <= limit).then_some(sign * coordinate)
}

/// Compact equirectangular picker over coordinates published by installed
/// tzdata. It complements rather than replaces the searchable selector: the
/// latter remains the complete source for aliases and keyboard/text search.
struct TimeZoneMap {
    points: Vec<TimeZonePoint>,
    selected: String,
    cursor: Option<usize>,
    hover: Option<usize>,
    last_cursor: Point,
    focused: bool,
    hit_radius: f64,
    action: Option<ui::widget::ActionEmitter<String>>,
    rect: Rect,
    /// [`Self::land`]'s cache and the plot it was projected into.
    land_plot: Rect,
    land_path: BezPath,
    /// The plot's inset inside the well, and the room below it for the
    /// caption naming the hovered or chosen zone; theme spacing.
    inset: f64,
    caption: f64,
}

/// The latitudes drawn, in degrees: everything people live at, without the
/// polar band that would take a third of an equirectangular map's height.
const MAP_LATITUDES: std::ops::RangeInclusive<f64> = -58.0..=84.0;
const MAP_SOUTH: f64 = *MAP_LATITUDES.start();
const MAP_NORTH: f64 = *MAP_LATITUDES.end();
/// The well's width over its height: the plot's own 360:142 degrees, less
/// the caption's share.
const MAP_ASPECT: f32 = 2.2;

/// The land masses, from Natural Earth's 1:110m land polygons
/// (`data/world-land.b64`, format and provenance in `data/README.md`).
/// tzdata publishes where each zone's city is, but nothing installed
/// describes the coastlines the cities sit on.
const WORLD_LAND: &str = include_str!("../../data/world-land.b64");

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
fn walk_world_land(mut visit: impl FnMut(f64, f64, bool)) {
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

impl TimeZoneMap {
    fn new(points: Vec<TimeZonePoint>, selected: impl Into<String>) -> Self {
        let selected = selected.into();
        let cursor = points.iter().position(|point| point.id == selected);
        Self {
            points,
            selected,
            cursor,
            hover: None,
            last_cursor: Point::ZERO,
            focused: false,
            hit_radius: f64::from(ui::tokens::Metrics::BASE.min_pointer_target) / 2.0,
            action: None,
            rect: Rect::ZERO,
            land_plot: Rect::ZERO,
            land_path: BezPath::new(),
            inset: f64::from(ui::tokens::Spacing::BASE.s2),
            caption: f64::from(ui::tokens::Spacing::BASE.s8),
        }
    }

    fn on_action(mut self, binding: Option<tilo_actions::ActionBinding<String>>) -> Self {
        self.action = binding.map(ui::widget::ActionEmitter::new);
        self
    }

    /// The equirectangular plot: one degree of longitude is as wide as one
    /// degree of latitude is tall, centred in the well above the caption,
    /// so the continents keep their shape at every window width.
    fn plot_rect(&self) -> Rect {
        let available = Rect::new(
            self.rect.x0 + self.inset,
            self.rect.y0 + self.inset,
            (self.rect.x1 - self.inset).max(self.rect.x0 + self.inset + 1.0),
            (self.rect.y1 - self.caption).max(self.rect.y0 + self.inset + 1.0),
        );
        let aspect = 360.0 / (MAP_NORTH - MAP_SOUTH);
        let width = available.width().min(available.height() * aspect);
        let height = width / aspect;
        let x0 = available.center().x - width / 2.0;
        let y0 = available.center().y - height / 2.0;
        Rect::new(x0, y0, x0 + width, y0 + height)
    }

    fn project(plot: Rect, longitude: f64, latitude: f64) -> Point {
        Point::new(
            plot.x0 + ((longitude + 180.0) / 360.0) * plot.width(),
            plot.y0 + ((MAP_NORTH - latitude) / (MAP_NORTH - MAP_SOUTH)) * plot.height(),
        )
    }

    fn projected(&self, index: usize) -> Point {
        let Some(point) = self.points.get(index) else {
            return Point::ZERO;
        };
        Self::project(self.plot_rect(), point.longitude(), point.latitude())
    }

    /// Whether the zone's city lies inside the drawn latitudes. Antarctic
    /// stations fall south of the map and stay choosable in the selector.
    fn on_map(&self, index: usize) -> bool {
        self.points
            .get(index)
            .is_some_and(|point| MAP_LATITUDES.contains(&point.latitude()))
    }

    /// The coastlines of [`WORLD_LAND`] in plot coordinates. Rebuilt only
    /// when the plot moves or resizes, not every frame.
    fn land(&mut self) -> &BezPath {
        let plot = self.plot_rect();
        if self.land_plot != plot {
            self.land_plot = plot;
            let mut path = BezPath::new();
            // The data leaves out Antarctica, south of the plot; any other
            // point is pinned inside it, so nothing draws over the well.
            walk_world_land(|longitude, latitude, starts| {
                let point = Self::project(plot, longitude, latitude.clamp(MAP_SOUTH, MAP_NORTH));
                if starts {
                    if !path.elements().is_empty() {
                        path.close_path();
                    }
                    path.move_to(point);
                } else {
                    path.line_to(point);
                }
            });
            if !path.elements().is_empty() {
                path.close_path();
            }
            self.land_path = path;
        }
        &self.land_path
    }

    fn point_at(&self, position: Point) -> Option<usize> {
        if !self.rect.contains(position) {
            return None;
        }
        self.points
            .iter()
            .enumerate()
            .filter(|(index, _)| self.on_map(*index))
            .filter_map(|(index, _)| {
                let projected = self.projected(index);
                let distance = (projected.x - position.x).hypot(projected.y - position.y);
                (distance <= self.hit_radius).then_some((index, distance))
            })
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(index, _)| index)
    }

    fn selected_index(&self) -> Option<usize> {
        self.points
            .iter()
            .position(|point| point.id == self.selected)
    }

    fn move_cursor(&mut self, horizontal: f64, vertical: f64) {
        if self.points.is_empty() {
            return;
        }
        let current = self
            .cursor
            .or_else(|| self.selected_index())
            .unwrap_or_default();
        let Some(origin) = self.points.get(current) else {
            return;
        };
        let target = self
            .points
            .iter()
            .enumerate()
            .filter(|(index, _)| *index != current && self.on_map(*index))
            .filter_map(|(index, candidate)| {
                let dx = candidate.longitude() - origin.longitude();
                // Positive screen-y means south, hence the inversion.
                let dy = origin.latitude() - candidate.latitude();
                let along = dx * horizontal + dy * vertical;
                if along <= 0.0 {
                    return None;
                }
                let across = (dx * vertical - dy * horizontal).abs();
                // Strongly favour the requested half-plane but avoid large
                // perpendicular jumps when a nearer neighbour is available.
                Some((index, along + across * 2.5))
            })
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(index, _)| index);
        if let Some(target) = target {
            self.cursor = Some(target);
        }
    }

    fn commit(&mut self, index: usize) {
        if let Some(point) = self.points.get(index) {
            if self.selected == point.id {
                return;
            }
            self.selected.clone_from(&point.id);
            self.cursor = Some(index);
            if let Some(action) = &mut self.action {
                let _ = action.emit(point.id.clone());
            }
        }
    }

    fn active_label(&self) -> String {
        let active = self.hover.or_else(|| self.cursor.filter(|_| self.focused));
        active.and_then(|index| self.points.get(index)).map_or_else(
            || {
                t_args(
                    "time-map-selected",
                    &[("zone", ArgValue::from(self.selected.as_str()))],
                )
            },
            |point| format!("{} — {}", point.city, point.region),
        )
    }
}

impl Widget for TimeZoneMap {
    ui::provides!(dyn ui::widget::controls::ActionControls);

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.hit_radius = f64::from(theme.tokens.metrics.min_pointer_target) / 2.0;
        self.inset = f64::from(theme.tokens.spacing.s2);
        self.caption = f64::from(theme.tokens.spacing.s8);
        // The height follows the width, so the plot keeps the continents'
        // proportions instead of squashing them into a fixed-height strip.
        let style = Style {
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::auto(),
            },
            aspect_ratio: Some(MAP_ASPECT),
            // Scales to any width, so it never sets its column's minimum.
            min_size: Size {
                width: Dimension::length(0.0),
                height: Dimension::auto(),
            },
            flex_shrink: 0.0,
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    ui::rect_geometry!();

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &Theme,
        _presence: &ui::presence::PresenceCtx,
    ) {
        // The map is a base-surface well inside its raised card: the fill is
        // the contrast step, and the rest edge derives from that fill
        // (`ui::surface::card`, HIG §9.5) so it registers on the dark preset
        // where a declared `border_standard` stroke did not. Focus replaces
        // the hairline with the 2 px focus ring.
        let radius = f64::from(theme.tokens.radii.panel);
        ui::surface::panel(scene, transform, self.rect, radius, theme);
        if self.focused {
            scene.stroke(
                &Stroke::new(2.0),
                transform,
                theme.border_focus,
                None,
                &RoundedRect::from_rect(self.rect, radius),
            );
        }

        let plot = self.plot_rect();
        // Graticule lines are the same derived hairline as the well's edge,
        // so they read as one surface's ruling rather than a second border;
        // one path, under the land.
        let mut graticule = BezPath::new();
        for longitude in [-120.0, -60.0, 0.0, 60.0, 120.0] {
            let x = Self::project(plot, longitude, 0.0).x;
            graticule.move_to((x, plot.y0));
            graticule.line_to((x, plot.y1));
        }
        for latitude in [-45.0, 0.0, 45.0] {
            let y = Self::project(plot, 0.0, latitude).y;
            graticule.move_to((plot.x0, y));
            graticule.line_to((plot.x1, y));
        }
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            theme.tokens.colors.border_standard,
            None,
            &graticule,
        );

        // Land is a tint of the well rather than a colour of its own, so the
        // map reads in every theme and covers the graticule; the zone dots
        // and the chosen city stay the strongest marks on it.
        let land_fill = theme.tokens.colors.overlay_medium;
        let land = self.land();
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            land_fill,
            None,
            land,
        );

        if self.points.is_empty() {
            text_renderer.draw_style(
                scene,
                &t("time-map-unavailable"),
                theme.text_dim,
                transform * Affine::translate((plot.x0 + 8.0, plot.center().y - 6.0)),
                Some((plot.width() - 16.0).max(1.0) as f32),
                &theme.text(ui::TextRole::Caption),
            );
            return;
        }

        let selected = self.selected_index();
        for index in (0..self.points.len()).filter(|index| self.on_map(*index)) {
            let centre = self.projected(index);
            let is_selected = selected == Some(index);
            let is_active =
                self.hover == Some(index) || (self.focused && self.cursor == Some(index));
            let radius = if is_selected { 4.5 } else { 2.2 };
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                if is_selected {
                    theme.tokens.colors.text_primary
                } else {
                    theme.text_dim.multiply_alpha(170.0 / 255.0)
                },
                None,
                &Circle::new(centre, radius),
            );
            // A ring marks the city under the pointer or the keyboard and,
            // in the text colour, the chosen one, so it can be found among
            // the others.
            if is_active || is_selected {
                scene.stroke(
                    &Stroke::new(2.0),
                    transform,
                    if is_active {
                        theme.border_focus
                    } else {
                        theme.tokens.colors.text_primary
                    },
                    None,
                    &Circle::new(centre, radius + 4.0),
                );
            }
        }

        text_renderer.draw_style(
            scene,
            &self.active_label(),
            theme.text_primary,
            transform * Affine::translate((self.rect.x0 + 10.0, self.rect.y1 - 23.0)),
            Some((self.rect.width() - 20.0).max(1.0) as f32),
            &theme.text(ui::TextRole::Caption),
        );
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor = ctx.logical(position);
                let hover = self.point_at(self.last_cursor);
                if hover != self.hover {
                    self.hover = hover;
                    *ctx.handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if let Some(index) = self.point_at(self.last_cursor) {
                    self.commit(index);
                    *ctx.focus_target = Some(ctx.node_id);
                    *ctx.handled = true;
                }
            }
            WindowEvent::KeyboardInput { event, .. }
                if self.focused && event.state == ElementState::Pressed =>
            {
                use winit::keyboard::{Key, NamedKey};
                match event.logical_key {
                    Key::Named(NamedKey::ArrowLeft) => self.move_cursor(-1.0, 0.0),
                    Key::Named(NamedKey::ArrowRight) => self.move_cursor(1.0, 0.0),
                    Key::Named(NamedKey::ArrowUp) => self.move_cursor(0.0, -1.0),
                    Key::Named(NamedKey::ArrowDown) => self.move_cursor(0.0, 1.0),
                    Key::Named(NamedKey::Enter | NamedKey::Space) => {
                        if let Some(index) = self.cursor {
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
        "TimeZoneMap"
    }

    fn test_id(&self) -> Option<&str> {
        Some("time.timezone-map")
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        self.point_at(point)
            .map(|_| winit::window::CursorIcon::Pointer)
    }

    fn tooltip(&self) -> Option<&str> {
        self.hover
            .and_then(|index| self.points.get(index))
            .map(|point| point.id.as_str())
    }

    fn is_focusable(&self) -> bool {
        !self.points.is_empty()
    }

    fn set_focus(&mut self, focused: bool) {
        self.focused = focused;
        if focused && self.cursor.is_none() {
            self.cursor = self.selected_index().or(Some(0));
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
                    self.cursor
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
                if let Some(index) = self.cursor.or_else(|| self.selected_index()) {
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

impl ui::widget::controls::ActionControls for TimeZoneMap {
    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        self.action
            .as_mut()
            .and_then(ui::widget::ActionEmitter::take)
    }
}

/// The zone a capture shows.
const SAMPLE_TIMEZONE: &str = "Europe/Amsterdam";

/// Read the configured time zone from `/etc/timezone` or the
/// `/etc/localtime` link; `None` when neither names one.
fn timezone() -> Option<String> {
    for p in ["/etc/timezone"] {
        if let Ok(s) = fs::read_to_string(Path::new(p)) {
            let t = s.trim();
            if !t.is_empty() {
                return Some(t.to_string());
            }
        }
    }
    // `/etc/localtime` is usually a symlink into zoneinfo; derive the zone
    // from its target when `/etc/timezone` is absent (common on Fedora).
    if let Ok(target) = fs::read_link("/etc/localtime") {
        let target = target.to_string_lossy();
        if let Some(idx) = target.find("zoneinfo/")
            && let Some(zone) = target.get(idx + "zoneinfo/".len()..)
        {
            return Some(zone.to_string());
        }
    }
    None
}

/// This device's clock, as the pane and the effects reach it: timedated in a
/// window someone uses, a fixed stand-in in a screenshot.
pub trait Clock: Send + Sync {
    /// What timedated reports: the zone, automatic time, synchronisation.
    ///
    /// # Errors
    ///
    /// Why timedated could not be asked.
    fn status(&self) -> Result<os_actions::TimeSyncStatus, String>;
    /// Whether automatic time is on, asked on its own when the status did
    /// not say.
    ///
    /// # Errors
    ///
    /// Why it could not be asked.
    fn ntp(&self) -> Result<bool, String>;
    /// The zones timedated accepts.
    ///
    /// # Errors
    ///
    /// Why the zone list could not be read.
    fn zones(&self) -> Result<Vec<String>, String>;
    /// The time and the date now, formatted, or why each could not be read.
    fn now(&self, h24: bool) -> (String, String);
    /// # Errors
    ///
    /// The underlying `timedatectl set-ntp` failure text when automatic time
    /// could not be changed (e.g. denied by policy).
    fn set_ntp(&self, enabled: bool) -> Result<(), String>;
    /// # Errors
    ///
    /// The underlying `timedatectl set-timezone` failure text.
    fn set_timezone(&self, timezone: &str) -> Result<(), String>;
}

/// The device's clock through timedated.
pub struct SystemClock;

impl Clock for SystemClock {
    fn status(&self) -> Result<os_actions::TimeSyncStatus, String> {
        crate::device_read::off_ui_thread();
        let mut status = os_actions::get_time_sync_status()?;
        if status.timezone.is_none() {
            status.timezone = timezone();
        }
        Ok(status)
    }

    fn ntp(&self) -> Result<bool, String> {
        crate::device_read::off_ui_thread();
        os_actions::get_ntp()
    }

    fn zones(&self) -> Result<Vec<String>, String> {
        crate::device_read::off_ui_thread();
        os_actions::list_timezones()
    }

    fn now(&self, h24: bool) -> (String, String) {
        // The toolkit's clock, which every clock display follows.
        let moment = ui::clock::local_now();
        let style = if h24 { TimeStyle::H24 } else { TimeStyle::H12 };
        (
            style.format(&moment),
            moment.format("%A, %-d %B %Y").to_string(),
        )
    }

    fn set_ntp(&self, enabled: bool) -> Result<(), String> {
        crate::device_read::off_ui_thread();
        os_actions::set_ntp(enabled)
    }

    fn set_timezone(&self, timezone: &str) -> Result<(), String> {
        crate::device_read::off_ui_thread();
        os_actions::set_timezone(timezone)
    }
}

/// The clock a capture shows: a fixed time in a fixed zone, which the
/// effects change in memory only.
pub struct SampleClock {
    status: Mutex<os_actions::TimeSyncStatus>,
}

impl Default for SampleClock {
    fn default() -> Self {
        Self {
            status: Mutex::new(os_actions::TimeSyncStatus {
                timezone: Some(SAMPLE_TIMEZONE.to_owned()),
                ntp_enabled: Some(true),
                synchronized: Some(true),
                source: Some("time.example.org".to_owned()),
            }),
        }
    }
}

impl Clock for SampleClock {
    fn status(&self) -> Result<os_actions::TimeSyncStatus, String> {
        crate::device_read::off_ui_thread();
        Ok(self.status.lock_recover().clone())
    }

    fn ntp(&self) -> Result<bool, String> {
        crate::device_read::off_ui_thread();
        Ok(self.status.lock_recover().ntp_enabled.unwrap_or(true))
    }

    fn zones(&self) -> Result<Vec<String>, String> {
        crate::device_read::off_ui_thread();
        Ok([
            "America/New_York",
            "Asia/Tokyo",
            "Europe/Amsterdam",
            "Europe/Berlin",
        ]
        .map(str::to_owned)
        .to_vec())
    }

    fn now(&self, h24: bool) -> (String, String) {
        let time = if h24 { "14:05" } else { "2:05 PM" };
        (time.to_owned(), "Wednesday 15 March 2023".to_owned())
    }

    fn set_ntp(&self, enabled: bool) -> Result<(), String> {
        crate::device_read::off_ui_thread();
        self.status.lock_recover().ntp_enabled = Some(enabled);
        Ok(())
    }

    fn set_timezone(&self, timezone: &str) -> Result<(), String> {
        crate::device_read::off_ui_thread();
        self.status.lock_recover().timezone = Some(timezone.to_owned());
        Ok(())
    }
}

/// Automatic time, the time zone and the clock format.
#[derive(Clone, Debug, PartialEq, Eq, tilo_resource::Effect)]
#[effect(operation = "settings.effect")]
pub enum TimeDateEffect {
    /// Set this device's time automatically from the network, or stop.
    #[effect(job, idempotent)]
    SetAutomaticTime { on: bool },
    /// Set this device's time zone.
    #[effect(job, idempotent)]
    SetTimeZone {
        #[param("An IANA time zone, such as `Europe/Amsterdam`")]
        zone: String,
    },
}

/// What the clock reports: timedated's status, the zones it accepts and
/// where they lie on the map. `timedatectl` goes through the system bus,
/// which can take up to a D-Bus timeout to answer, so this is read off the
/// UI thread ([`Reading`]).
#[derive(Clone)]
struct ClockReading {
    /// What timedated reports, or why it could not be asked.
    status: Result<os_actions::TimeSyncStatus, String>,
    /// Whether automatic time is on, as the status or the separate ask
    /// said.
    automatic: Option<bool>,
    /// Why the automatic-time switch's position is unknown.
    auto_error: Option<String>,
    /// The zones to choose from, the configured one among them, or why the
    /// list could not be read.
    zones: Result<Vec<String>, String>,
    points: Vec<TimeZonePoint>,
}

impl ClockReading {
    fn read(clock: &dyn Clock) -> Self {
        let status = clock.status();
        // Automatic time is asked on its own when timedated's status did
        // not say, whether or not the status could be read.
        let mut automatic = status.as_ref().ok().and_then(|status| status.ntp_enabled);
        let mut auto_error = None;
        if automatic.is_none() {
            match clock.ntp() {
                Ok(enabled) => automatic = Some(enabled),
                Err(error) => auto_error = Some(error),
            }
        }
        let zones = clock.zones().map(|mut zones| {
            if let Some(zone) = status
                .as_ref()
                .ok()
                .and_then(|status| status.timezone.as_ref())
                && !zones.contains(zone)
            {
                zones.push(zone.clone());
                zones.sort();
            }
            zones
        });
        let points = zones.as_deref().map(timezone_map).unwrap_or_default();
        Self {
            status,
            automatic,
            auto_error,
            zones,
            points,
        }
    }
}

/// What the Time & Date area keeps for the window: the device's clock, what
/// it last reported, and where its last change stands.
pub struct TimeDateArea {
    clock: Arc<dyn Clock>,
    /// What the clock last reported, read again off the UI thread whenever
    /// the pane opens and after every change.
    reading: Reading<ClockReading>,
    /// Why the last change failed, for the pane to show.
    notice: LocalState<Option<String>>,
}

impl TimeDateArea {
    /// The area over this device's clock, or a stand-in for a capture.
    /// Nothing is read until the pane opens.
    pub fn new(cx: &PaneCx) -> Self {
        let clock: Arc<dyn Clock> = if cx.reaches_devices() {
            Arc::new(SystemClock)
        } else {
            Arc::new(SampleClock::default())
        };
        Self::over(clock)
    }

    fn over(clock: Arc<dyn Clock>) -> Self {
        let read = Arc::clone(&clock);
        Self {
            clock,
            reading: Reading::new(move || ClockReading::read(read.as_ref())),
            notice: LocalState::new(None),
        }
    }
}

/// Answer `effect`. Device changes run off the UI thread: timedated may ask
/// for authorisation.
#[expect(
    clippy::unnecessary_wraps,
    reason = "every area of Settings answers its effects with one signature"
)]
pub(crate) fn answer(
    app: &SettingsApp,
    effect: TimeDateEffect,
    cx: &mut EffectCx<Settings, SettingsApp>,
) -> EffectAnswer {
    let clock = Arc::clone(&app.time_date().clock);
    Ok(crate::device_read::job(
        cx,
        move || match effect {
            TimeDateEffect::SetAutomaticTime { on } => clock
                .set_ntp(on)
                .map_err(|error| format!("automatic time could not be changed: {error}")),
            TimeDateEffect::SetTimeZone { zone } => {
                let zones = clock
                    .zones()
                    .map_err(|error| format!("time zones could not be read: {error}"))?;
                if !zones.contains(&zone) {
                    return Err(format!("`{zone}` is not a time zone of this device"));
                }
                clock
                    .set_timezone(&zone)
                    .map_err(|error| format!("time zone could not be changed: {error}"))
            }
        },
        |app: &mut SettingsApp, outcome: Result<(), String>, _| {
            let area = app.time_date();
            // What the clock took is read off the UI thread too.
            area.reading.refresh();
            match outcome {
                Ok(()) => {
                    area.notice.set(None);
                    Ok(Effected::done())
                }
                Err(error) => {
                    area.notice.set(Some(error.clone()));
                    Err(error)
                }
            }
        },
    ))
}

/// Time & Date pane: what the device's clock reports, and the person's
/// clock format.
pub struct TimeDatePane {
    cx: PaneCx,
    clock: Arc<dyn Clock>,
    locale: Followed<Locale>,
    notice: LocalState<Option<String>>,
    notice_shown: Option<String>,
    /// What the clock reports, read off the UI thread.
    reading: Reading<ClockReading>,
    /// The revision of `reading` taken in.
    reading_seen: u64,
    /// Whether a reading has landed; until then the rows say the clock is
    /// being read.
    read: bool,
    timezone: String,
    timezones: Vec<String>,
    timezone_points: Vec<TimeZonePoint>,
    sync_status: os_actions::TimeSyncStatus,
    /// Why timedated could not be asked for its state.
    status_error: Option<String>,
    /// Why the zone list could not be read; there is then nothing to choose.
    timezones_error: Option<String>,
    /// The configured zone could not be found anywhere.
    timezone_unknown: bool,
    /// Why the automatic-time switch's position is unknown.
    auto_error: Option<String>,
    h24: bool,
    composed: ui::widget::Composed,
}

impl TimeDatePane {
    /// The pane over `area`'s clock and `cx`'s locale. It shows what the
    /// clock last reported, or that it is being read, and reads it again
    /// off the UI thread. Every part the clock could not read is kept as
    /// its failure and shown as one, never as a default.
    pub fn new(cx: PaneCx, area: &TimeDateArea) -> Self {
        let locale = cx.follow::<Locale>();
        area.reading.refresh();
        let mut pane = Self {
            h24: matches!(locale.get().time_format, TimeStyle::H24),
            cx,
            clock: Arc::clone(&area.clock),
            locale,
            notice: area.notice.clone(),
            notice_shown: area.notice.get(),
            reading: area.reading.clone(),
            reading_seen: area.reading.revision(),
            read: false,
            timezone: String::new(),
            timezones: Vec::new(),
            timezone_points: Vec::new(),
            sync_status: os_actions::TimeSyncStatus::default(),
            status_error: None,
            timezones_error: None,
            timezone_unknown: false,
            auto_error: None,
            composed: ui::widget::Composed::default(),
        };
        if let Some(reading) = area.reading.get() {
            pane.adopt(reading);
        }
        pane
    }

    /// Show what the clock reported.
    fn adopt(&mut self, reading: ClockReading) {
        self.read = true;
        (self.sync_status, self.status_error) = match reading.status {
            Ok(status) => (status, None),
            Err(error) => (os_actions::TimeSyncStatus::default(), Some(error)),
        };
        self.sync_status.ntp_enabled = reading.automatic;
        self.auto_error = reading.auto_error;
        self.timezone_unknown = self.sync_status.timezone.is_none();
        self.timezone = self.sync_status.timezone.clone().unwrap_or_default();
        (self.timezones, self.timezones_error) = match reading.zones {
            Ok(zones) => (zones, None),
            Err(error) => (Vec::new(), Some(error)),
        };
        self.timezone_points = reading.points;
    }

    /// The automatic-time switch, or why its position is unknown.
    fn auto_row(&self) -> Item {
        if !self.read {
            return value_row(&t("time-auto"), &t("time-reading"));
        }
        match (&self.auto_error, self.sync_status.ntp_enabled) {
            (Some(error), _) => error_row(
                &t("time-auto"),
                &t_args("time-status-error", &[("error", error.as_str().into())]),
            ),
            (None, automatic) => self.cx.row(t("time-auto")).id("time.auto").toggle_effect(
                &LocalState::new(automatic.unwrap_or(false)),
                effect(TimeDateEffect::SET_AUTOMATIC_TIME),
            ),
        }
    }

    /// The zone chooser, or why there is nothing to choose from.
    fn zone_row(&self) -> Item {
        if !self.read {
            return value_row(&t("time-timezone"), &t("time-reading"));
        }
        match &self.timezones_error {
            Some(error) => error_row(
                &t("time-timezone"),
                &t_args("time-zones-error", &[("error", error.as_str().into())]),
            ),
            None => self
                .cx
                .row(t("time-timezone"))
                .id("time.timezone")
                .choice_effect(
                    self.timezones.clone(),
                    &LocalState::new(self.timezone.clone()),
                    effect(TimeDateEffect::SET_TIME_ZONE),
                ),
        }
    }

    fn build_composition(&self) -> Box<dyn Widget> {
        let (now, today) = self.clock.now(self.h24);
        let now = value_row(&t("time-current-time"), &now);
        let today = value_row(&t("time-current-date"), &today);
        let auto_row = self.auto_row();
        let zone_row = self.zone_row();

        let clock_format = if self.h24 {
            t("time-clock-24h")
        } else {
            t("time-clock-12h")
        };
        let automatic = self.sync_status.ntp_enabled.unwrap_or(false);
        let sync = match self.sync_status.synchronized {
            Some(true) => t("time-sync-ok"),
            Some(false) if automatic => t("time-sync-waiting"),
            Some(false) => t("time-sync-disabled"),
            None => t("time-sync-unknown"),
        };
        let source_name = self
            .sync_status
            .source
            .clone()
            .unwrap_or_else(|| t("time-source-unknown"));
        let mapped = self.timezone_points.len();
        let map_note = if !self.read {
            t("time-reading")
        } else if mapped == 0 {
            t("time-map-empty")
        } else {
            t_args(
                "time-map-count",
                &[(
                    "mapped",
                    ArgValue::from(i64::try_from(mapped).unwrap_or(i64::MAX)),
                )],
            )
        };

        let clock = card(
            &t("time-clock-title"),
            rows(vec![
                auto_row,
                setting_row(
                    &t("time-24h"),
                    Box::new(self.cx.bind_field_with(
                        Toggle::from_state(&LocalState::new(self.h24)).with_id("time.h24"),
                        Locale::paths().time_format,
                        |on: bool| Ok(if on { TimeStyle::H24 } else { TimeStyle::H12 }),
                    )),
                ),
                zone_row,
                now,
                today,
            ]),
        );

        let formats = card(
            &t("time-display-title"),
            rows(vec![
                value_row(&t("time-clock-format"), &clock_format),
                note(&t("time-display-caption")),
            ]),
        );

        let map = card(
            &t("time-map-title"),
            rows(vec![
                block(Box::new(
                    TimeZoneMap::new(self.timezone_points.clone(), self.timezone.clone())
                        .on_action(self.cx.binding(effect(TimeDateEffect::SET_TIME_ZONE))),
                )),
                note(&map_note),
            ]),
        );

        let source = card(
            &t("time-source-title"),
            rows(match &self.status_error {
                _ if !self.read => vec![
                    value_row(&t("time-sync"), &t("time-reading")),
                    note(&t("time-source-caption")),
                ],
                Some(error) => vec![
                    error_row(
                        &t("time-sync"),
                        &t_args("time-status-error", &[("error", error.as_str().into())]),
                    ),
                    note(&t("time-source-caption")),
                ],
                None => vec![
                    value_row(&t("time-sync"), &sync),
                    value_row(&t("time-source"), &source_name),
                    note(&t("time-source-caption")),
                ],
            }),
        );

        let mut cards = vec![clock, map, source, formats];
        if let Some(notice) = &self.notice_shown {
            cards.insert(0, card(&t("time-notice-title"), rows(vec![note(notice)])));
        }
        crate::widgets::cards(cards)
    }
}

impl Widget for TimeDatePane {
    fn sync(&mut self, state: &AppModel, root: &ModelPath) {
        let revision = self.reading.revision();
        if revision != self.reading_seen {
            self.reading_seen = revision;
            if let Some(reading) = self.reading.get() {
                self.adopt(reading);
            }
            self.composed.invalidate();
        }
        let notice = self.notice.get();
        if notice != self.notice_shown {
            self.notice_shown = notice;
            self.composed.invalidate();
        }
        if self.locale.refresh() {
            self.h24 = matches!(self.locale.get().time_format, TimeStyle::H24);
            self.composed.invalidate();
        }
        self.composed.sync(state, root);
    }

    crate::panes::composed_pane!();

    fn name(&self) -> &'static str {
        "TimeDatePane"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::SettingsEffect;
    use crate::resource::SettingsShell;
    use taffy::prelude::TaffyMaxContent;
    use ui::prelude::TestApp;

    fn fixture_points() -> Vec<TimeZonePoint> {
        let zones = vec![
            "Europe/Amsterdam".to_string(),
            "Europe/Berlin".to_string(),
            "America/New_York".to_string(),
        ];
        timezone_map_from_tab(
            &zones,
            "# installed tzdata fixture\n\
             NL\t+5222+00454\tEurope/Amsterdam\n\
             DE\t+5230+01322\tEurope/Berlin\n\
             US\t+404251-0740023\tAmerica/New_York\n\
             GB\t+513030-0000731\tEurope/London\n",
        )
    }

    /// The packed land data decodes whole: every ring, every point on the
    /// globe, nothing south of the map.
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
    }

    /// A zone merged out of zone1970.tab (Amsterdam, only in zone.tab) still
    /// gets its dot; a zone in both tables keeps zone1970.tab's coordinates.
    #[test]
    fn a_zone_merged_out_of_zone1970_is_still_on_the_map() {
        // Case `a_zone_merged_out_of_zone1970_is_still_on_the_map`: the
        // device's own zone (here Amsterdam, only in zone.tab) gets its dot;
        // a zone in both tables keeps zone1970.tab's coordinates.
        {
            let zones = vec![
                "Europe/Amsterdam".to_string(),
                "Europe/Brussels".to_string(),
            ];
            let points = timezone_map_from_tab(
                &zones,
                "BE,LU,NL\t+5050+00420\tEurope/Brussels\n\
                 NL\t+5222+00454\tEurope/Amsterdam\n\
                 BE\t+0000+00000\tEurope/Brussels\n",
            );
            let brussels = points
                .iter()
                .find(|point| point.id == "Europe/Brussels")
                .expect("Brussels is mapped");
            assert_eq!(brussels.latitude_e6, 50_833_333);
            assert!(points.iter().any(|point| point.id == "Europe/Amsterdam"));
        }
    }

    /// The map draws the land, keeps its proportions and stays inside its
    /// well at a narrow and a wide width.
    #[test]
    fn the_map_draws_the_land_in_proportion_at_every_width() {
        // Case `map_projects_points_inside_normal_and_narrow_layouts`.
        {
            for width in [280.0, 720.0] {
                let mut map = TimeZoneMap::new(fixture_points(), "Europe/Amsterdam");
                let theme = Theme::dark();
                let mut taffy = TaffyTree::new();
                let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
                let node = map.build_layout(&mut taffy, &theme, &mut node_map);
                let root = taffy
                    .new_with_children(
                        Style {
                            size: Size {
                                width: Dimension::length(width),
                                height: Dimension::auto(),
                            },
                            ..Default::default()
                        },
                        &[node],
                    )
                    .unwrap();
                taffy.compute_layout(root, Size::MAX_CONTENT).unwrap();
                map.apply_layout(&taffy, node, Point::ZERO);

                assert!((map.rect.width() - f64::from(width)).abs() < 0.1);
                assert!(
                    (map.rect.height() - f64::from(width) / f64::from(MAP_ASPECT)).abs() < 0.5,
                    "the map's height follows its width at {width}"
                );
                let plot = map.plot_rect();
                assert!(
                    (plot.width() / plot.height() - 360.0 / (MAP_NORTH - MAP_SOUTH)).abs() < 0.01,
                    "a degree is as wide as it is tall at {width}: {plot:?}"
                );
                assert!(
                    plot.width() > map.rect.width() * 0.75,
                    "the plot uses the well at {width}: {plot:?} in {:?}",
                    map.rect
                );
                for index in 0..map.points.len() {
                    assert!(map.plot_rect().contains(map.projected(index)));
                }
                // The user saw an empty grid: the map must draw the land the
                // zones lie on, all of it inside the plot.
                let land = map.land().clone();
                let bounds = vello::kurbo::Shape::bounding_box(&land);
                assert!(
                    land.elements().len() > 1000,
                    "the coastlines are drawn at {width}"
                );
                assert!(
                    bounds.width() > plot.width() * 0.95 && bounds.height() > plot.height() * 0.9,
                    "the land spans the plot at {width}: {bounds:?} in {plot:?}"
                );
                assert!(
                    plot.inflate(0.01, 0.01).contains_rect(bounds),
                    "no land outside the plot at {width}: {bounds:?} in {plot:?}"
                );
                // Berlin lies well inland.
                let berlin = map
                    .points
                    .iter()
                    .position(|point| point.id == "Europe/Berlin")
                    .expect("fixture zone");
                assert!(
                    vello::kurbo::Shape::contains(&land, map.projected(berlin)),
                    "Berlin's dot lies on the drawn land at {width}"
                );
            }
        }
    }

    /// tzdata parsing, keyboard access and non-empty timezones.
    #[test]
    fn timezone_map_parses_projects_and_commits_known_zones() {
        // Case `tzdata_coordinates_are_parsed_and_filtered_by_timedated_ids`.
        {
            let points = fixture_points();
            assert_eq!(
                points
                    .iter()
                    .map(|point| point.id.as_str())
                    .collect::<Vec<_>>(),
                ["America/New_York", "Europe/Amsterdam", "Europe/Berlin"]
            );
            let new_york = &points[0];
            assert_eq!(new_york.latitude_e6, 40_714_167);
            assert_eq!(new_york.longitude_e6, -74_006_389);
            assert!(
                points.iter().all(|point| point.id != "Europe/London"),
                "a coordinate-table entry not accepted by timedated must not enter the map"
            );
            assert_eq!(parse_iso6709_coordinate("+9001+00000"), None);
            assert_eq!(parse_iso6709_coordinate("garbage"), None);
        }

        // Case `map_is_keyboard_accessible_and_only_offers_known_zones`.
        {
            let mut map = TimeZoneMap::new(fixture_points(), "Europe/Amsterdam");
            map.set_focus(true);
            let access = map.accessibility().expect("map semantics");
            assert_eq!(access.role, ui::a11y::Role::Canvas);
            assert!(access.state.focusable);
            assert!(access.state.focused);

            let mut doc = ui::automerge::AutoCommit::new();
            let root = ui::automerge::ROOT;
            assert!(map.perform_action(
                &ui::a11y::Action::SetValue("Europe/Berlin".to_string()),
                &[],
                &mut ui::widget::Edits::lend(&mut doc),
                &root,
            ));
            assert_eq!(map.selected, "Europe/Berlin");
            assert!(!map.perform_action(
                &ui::a11y::Action::SetValue("Mars/Olympus_Mons".to_string()),
                &[],
                &mut ui::widget::Edits::lend(&mut doc),
                &root,
            ));
        }
    }

    /// A clock whose timedated cannot be asked: no `timedatectl`, no system
    /// bus.
    struct UnreadableClock;

    impl Clock for UnreadableClock {
        fn status(&self) -> Result<os_actions::TimeSyncStatus, String> {
            Err("timedatectl not installed".into())
        }
        fn ntp(&self) -> Result<bool, String> {
            Err("timedatectl not installed".into())
        }
        fn zones(&self) -> Result<Vec<String>, String> {
            Err("timedatectl not installed".into())
        }
        fn now(&self, _: bool) -> (String, String) {
            ("14:05".into(), "Wednesday 15 March 2023".into())
        }
        fn set_ntp(&self, _: bool) -> Result<(), String> {
            Err("timedatectl not installed".into())
        }
        fn set_timezone(&self, _: &str) -> Result<(), String> {
            Err("timedatectl not installed".into())
        }
    }

    /// A host whose timedated cannot be asked shows each unread value as
    /// that failure: the automatic-time switch is not offered at a guessed
    /// position, the zone is not a sample zone and synchronisation is not
    /// "unknown".
    #[test]
    fn unreadable_time_settings_are_shown_as_errors() {
        let profile = tempfile::tempdir().unwrap();
        let cx = PaneCx::in_file(profile.path().join("settings.json"));
        let area = TimeDateArea::over(Arc::new(UnreadableClock));
        let mut pane = TimeDatePane::new(cx, &area);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while area.reading.revision() == pane.reading_seen {
            assert!(std::time::Instant::now() < deadline, "the clock is read");
            std::thread::yield_now();
        }
        pane.adopt(area.reading.get().expect("a reading landed"));
        assert!(pane.timezone_unknown);
        let text = crate::widgets::announced_text(pane.build_composition().as_ref());
        let says = |needle: &str| text.iter().any(|line| line == needle);
        assert!(
            says("Can't read time settings: timedatectl not installed"),
            "{text:#?}"
        );
        assert!(
            says("Can't list time zones: timedatectl not installed"),
            "{text:#?}"
        );
        assert!(!says(&t("time-sync-unknown")), "{text:#?}");
        assert!(
            !text.iter().any(|line| line.contains(SAMPLE_TIMEZONE)),
            "{text:#?}"
        );
    }

    /// A clock whose timedated does not answer until it is let go (or ten
    /// seconds pass), as `timedatectl` does behind a filtered system bus
    /// that holds the call until the D-Bus timeout.
    struct StalledClock(Arc<(Mutex<bool>, std::sync::Condvar)>);

    impl StalledClock {
        fn wait(&self) {
            let (open, released) = &*self.0;
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            let mut open = open.lock().unwrap();
            while !*open && std::time::Instant::now() < deadline {
                let left = deadline.saturating_duration_since(std::time::Instant::now());
                open = released.wait_timeout(open, left).unwrap().0;
            }
            drop(open);
        }
    }

    impl Clock for StalledClock {
        fn status(&self) -> Result<os_actions::TimeSyncStatus, String> {
            self.wait();
            SampleClock::default().status()
        }
        fn ntp(&self) -> Result<bool, String> {
            self.wait();
            Ok(true)
        }
        fn zones(&self) -> Result<Vec<String>, String> {
            self.wait();
            SampleClock::default().zones()
        }
        fn now(&self, h24: bool) -> (String, String) {
            SampleClock::default().now(h24)
        }
        fn set_ntp(&self, _: bool) -> Result<(), String> {
            self.wait();
            Ok(())
        }
        fn set_timezone(&self, _: &str) -> Result<(), String> {
            self.wait();
            Ok(())
        }
    }

    /// Opening Time & Date while timedated takes its time is answered and
    /// drawn at once: the pane says the clock is being read, and shows the
    /// zone once timedated answers.
    #[test]
    fn a_stalled_timedated_never_holds_the_window() {
        ui::i18n::I18nManager::init("en-US");
        let mut app = TestApp::<SettingsShell>::new().unwrap();
        let gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        let stalled = Arc::clone(&gate);
        app.with_widget::<SettingsApp, _>(move |settings| {
            settings.set_time_date(TimeDateArea::over(Arc::new(StalledClock(stalled))));
        })
        .expect("the Settings window is mounted");

        let clicked = std::time::Instant::now();
        crate::effects::show_pane(&mut app, crate::categories::Category::TimeDate);
        app.harness()
            .poll_frame()
            .expect("the window draws its next frame");
        let answered = clicked.elapsed();
        assert!(
            answered < std::time::Duration::from_secs(1),
            "opening the pane waited {answered:?} on timedated"
        );
        let frame = app.harness().inspection_json().unwrap();
        assert!(frame.contains(&t("time-reading")), "{frame}");
        assert!(!frame.contains(SAMPLE_TIMEZONE), "{frame}");

        *gate.0.lock().unwrap() = true;
        gate.1.notify_all();
        assert!(
            app.harness()
                .wait_until(ui::time::Duration::from_secs(30), |harness| harness
                    .inspection_json()
                    .is_ok_and(|frame| frame.contains(SAMPLE_TIMEZONE))),
            "the zone timedated reports is shown"
        );
    }

    /// Automatic time and the zone are this device's, changed through its
    /// clock; a zone the device does not know is refused. The clock format
    /// is the person's locale, shared with Region & Formats.
    #[test]
    fn time_effects_reach_the_clock_and_the_locale() {
        let mut app = TestApp::<SettingsShell>::new().unwrap();
        app.effect(&SettingsEffect::from(TimeDateEffect::SetTimeZone {
            zone: "Europe/Berlin".into(),
        }))
        .expect("a known zone is applied");
        app.effect(&SettingsEffect::from(TimeDateEffect::SetAutomaticTime {
            on: false,
        }))
        .expect("automatic time is turned off");
        app.effect(&SettingsEffect::from(
            crate::effects::NavigationEffect::OpenPane {
                pane: "time_date".into(),
            },
        ))
        .unwrap();
        // The clock is read off the UI thread: the pane shows the zone once
        // the reading lands.
        assert!(
            app.harness()
                .wait_until(ui::time::Duration::from_secs(30), |harness| harness
                    .inspection_json()
                    .is_ok_and(|frame| frame.contains("Europe/Berlin"))),
            "the zone the clock took is shown"
        );
        assert!(
            app.effect(&SettingsEffect::from(TimeDateEffect::SetTimeZone {
                zone: "Mars/Olympus_Mons".into(),
            }))
            .is_err()
        );

        crate::host::testing::edit_field(&mut app, "settings.locale", "time_format", "H12")
            .expect("the clock format is the locale's");
        assert_eq!(app.settings::<Locale>().get().time_format, TimeStyle::H12);
        let frame = app.harness().inspection_json().unwrap();
        assert!(frame.contains("2:05 PM"), "{frame}");
    }
}
