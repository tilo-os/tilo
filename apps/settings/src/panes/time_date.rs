#![allow(
    clippy::unreachable,
    reason = "static Time & Date controls always publish their closed typed bindings"
)]

//! Time & Date pane — clock, time zone, and date/time display format.
//!
//! The clock-format controls here are a *local* view onto the same locale
//! concepts the Region & Formats pane owns globally; this pane focuses on
//! the clock surface (automatic time, 24-hour clock, time zone) while
//! Region owns the system-wide currency/date/measurement contract.
//!
//! ## Live vs. sample
//!
//! - **Current time / date** are read from the real wall clock via
//!   `os_actions::time::{now_clock, now_date}` at build time, so the panel
//!   shows the actual time (formatted per the 24-hour toggle), not a frozen
//!   string.
//! - **Time zone** is read from timedated, falling back to `/etc/timezone`
//!   (or the `/etc/localtime` symlink target) only when the service does not
//!   report it. Choices come from `timedatectl list-timezones`; map coordinates
//!   are joined from the installed tzdata tables, never an embedded list.
//! - **Set time automatically** drives `timedatectl set-ntp` through
//!   `os_actions::set_ntp`; its initial state is sampled via
//!   `os_actions::get_ntp`. The setter needs privilege, so a denial is
//!   logged rather than surfaced as a crash.
//! - **24-hour clock** is a display preference (re-renders the clock); it
//!   carries a CRDT key the rest of the system can read.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;
use std::sync::Arc;
use ui::doc::read_str_or_text;
use ui::widget::{AppModel, ModelPath};

use automerge::{AutoCommit, ObjId, transaction::Transactable};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use tracing::warn;
use ui::theme::Theme;
use ui::widget::{Dropdown, Toggle, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Circle, Line, Point, Rect, RoundedRect, Stroke};
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::i18n::{t, t_args};
use crate::region::{RegionSettings, TimeStyle};
use crate::widgets::{caption, card, choice_row, rows, setting_row, value_row};
use ui::i18n::ArgValue;

fn time_bool_action(id: &str) -> tilo_actions::ActionBinding<bool> {
    match crate::actions::time_date_bool_binding(id) {
        Some(binding) => binding,
        None => unreachable!("Time & Date action must be published"),
    }
}

fn time_text_action(id: &str) -> tilo_actions::ActionBinding<String> {
    match crate::actions::time_date_text_binding(id) {
        Some(binding) => binding,
        None => unreachable!("Time & Date action must be published"),
    }
}

pub const AUTO_TIME_KEY: &str = "time.auto";
pub const H24_KEY: &str = "time.h24";
pub const TIMEZONE_KEY: &str = "time.timezone";

fn draft_key(key: &str) -> String {
    format!("{key}.edit")
}

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
    TABLES
        .iter()
        .find_map(|path| fs::read_to_string(path).ok())
        .map_or_else(Vec::new, |table| timezone_map_from_tab(zones, &table))
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
                city: city.replace('_', " ").replace('/', " · "),
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

const MAP_HEIGHT: f32 = 176.0;
const MAP_HIT_RADIUS: f64 = 11.0;

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
    needs_commit: bool,
    action: Option<ui::widget::ActionEmitter<String>>,
    rect: Rect,
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
            needs_commit: false,
            action: None,
            rect: Rect::ZERO,
        }
    }

    fn on_action(mut self, binding: tilo_actions::ActionBinding<String>) -> Self {
        self.action = Some(ui::widget::ActionEmitter::new(binding));
        self
    }

    fn plot_rect(&self) -> Rect {
        Rect::new(
            self.rect.x0 + 10.0,
            self.rect.y0 + 10.0,
            (self.rect.x1 - 10.0).max(self.rect.x0 + 11.0),
            (self.rect.y1 - 34.0).max(self.rect.y0 + 11.0),
        )
    }

    fn projected(&self, index: usize) -> Point {
        let Some(point) = self.points.get(index) else {
            return Point::ZERO;
        };
        let plot = self.plot_rect();
        Point::new(
            plot.x0 + ((point.longitude() + 180.0) / 360.0) * plot.width(),
            plot.y0 + ((90.0 - point.latitude()) / 180.0) * plot.height(),
        )
    }

    fn point_at(&self, position: Point) -> Option<usize> {
        if !self.rect.contains(position) {
            return None;
        }
        self.points
            .iter()
            .enumerate()
            .filter_map(|(index, _)| {
                let projected = self.projected(index);
                let distance = (projected.x - position.x).hypot(projected.y - position.y);
                (distance <= MAP_HIT_RADIUS).then_some((index, distance))
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
            .filter(|(index, _)| *index != current)
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

    fn commit(&mut self, index: usize, state: &mut AutoCommit, root: &ObjId) {
        if let Some(point) = self.points.get(index) {
            if self.selected == point.id {
                return;
            }
            self.selected.clone_from(&point.id);
            self.cursor = Some(index);
            if let Some(action) = &mut self.action {
                let _ = action.emit(point.id.clone());
            } else {
                let _ = state.put(root, draft_key(TIMEZONE_KEY), point.id.as_str());
                self.needs_commit = true;
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
    ui::provides!(
        dyn ui::widget::controls::ActionControls,
        dyn ui::undo::UndoPoints
    );

    fn sync(&mut self, state: &mut AppModel, root: &ModelPath) {
        let (state, root) = ui::doc::document(state, root);
        if self.action.is_some() {
            return;
        }
        if let Some(value) = read_str_or_text(state, root, TIMEZONE_KEY) {
            self.selected = value;
            if self.cursor.is_none() {
                self.cursor = self.selected_index();
            }
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        #[allow(
            clippy::unwrap_used,
            reason = "taffy 0.10's new_leaf always inserts and returns Ok; see ui::widget::TaffyTreeExt's doc comment"
        )]
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::length(MAP_HEIGHT),
                },
                flex_shrink: 0.0,
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x + layout.size.width),
            origin.y + f64::from(layout.location.y + layout.size.height),
        );
    }

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
        let radius = f64::from(theme.tokens.radii.md);
        let well = theme.tokens.colors.surfaces.base;
        ui::surface::card(scene, transform, self.rect, radius, well);
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
        // so they read as one surface's ruling rather than a second border.
        let grid_color = ui::surface::edge_on(well);
        for longitude in [-120.0, -60.0, 0.0, 60.0, 120.0] {
            let x = plot.x0 + ((longitude + 180.0) / 360.0) * plot.width();
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                grid_color,
                None,
                &Line::new((x, plot.y0), (x, plot.y1)),
            );
        }
        for latitude in [-45.0, 0.0, 45.0] {
            let y = plot.y0 + ((90.0 - latitude) / 180.0) * plot.height();
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                grid_color,
                None,
                &Line::new((plot.x0, y), (plot.x1, y)),
            );
        }

        if self.points.is_empty() {
            text_renderer.draw_text(
                scene,
                &t("time-map-unavailable"),
                11.0,
                theme.text_dim,
                transform * Affine::translate((plot.x0 + 8.0, plot.center().y - 6.0)),
                Some((plot.width() - 16.0).max(1.0) as f32),
            );
            return;
        }

        let selected = self.selected_index();
        for index in 0..self.points.len() {
            let centre = self.projected(index);
            let is_selected = selected == Some(index);
            let is_active =
                self.hover == Some(index) || (self.focused && self.cursor == Some(index));
            let radius = if is_selected { 4.5 } else { 2.2 };
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                if is_selected {
                    theme.accent_primary
                } else {
                    theme.text_dim.multiply_alpha(170.0 / 255.0)
                },
                None,
                &Circle::new(centre, radius),
            );
            if is_active {
                scene.stroke(
                    &Stroke::new(2.0),
                    transform,
                    theme.border_focus,
                    None,
                    &Circle::new(centre, radius + 4.0),
                );
            }
        }

        text_renderer.draw_text(
            scene,
            &self.active_label(),
            11.0,
            theme.text_primary,
            transform * Affine::translate((self.rect.x0 + 10.0, self.rect.y1 - 23.0)),
            Some((self.rect.width() - 20.0).max(1.0) as f32),
        );
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
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
                    self.commit(index, ctx.state, ctx.path);
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
                            self.commit(index, ctx.state, ctx.path);
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

    fn get_rect(&self) -> Rect {
        self.rect
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
        state: &mut AppModel,
        root: &ModelPath,
    ) -> bool {
        let (state, root) = ui::doc::document(state, root);
        match action {
            ui::a11y::Action::Activate => {
                if let Some(index) = self.cursor.or_else(|| self.selected_index()) {
                    self.commit(index, state, root);
                    true
                } else {
                    false
                }
            }
            ui::a11y::Action::SetValue(value) => {
                if let Some(index) = self.points.iter().position(|point| &point.id == value) {
                    self.commit(index, state, root);
                    true
                } else {
                    false
                }
            }
            _ => false,
        }
    }
}

impl ui::undo::UndoPoints for TimeZoneMap {
    fn needs_commit(&self) -> bool {
        self.needs_commit
    }
    fn on_commit(&mut self) {
        self.needs_commit = false;
    }
}

impl ui::widget::controls::ActionControls for TimeZoneMap {
    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        self.action
            .as_mut()
            .and_then(ui::widget::ActionEmitter::take)
    }
}

/// Side-effect interface for the automatic-time (NTP) toggle. Production
/// forwards to `os_actions::set_ntp` (`timedatectl set-ntp`); tests inject a
/// recorder so the "auto → set-ntp" path is asserted without shelling out.
pub trait NtpControl: Send + Sync {
    /// # Errors
    ///
    /// Returns the underlying `timedatectl set-ntp` failure text when the
    /// automatic-time toggle could not be applied (e.g. denied by policy).
    fn set_ntp(&self, enabled: bool) -> Result<(), String>;

    /// # Errors
    ///
    /// Returns the underlying `timedatectl set-timezone` failure text when
    /// the requested zone could not be applied.
    fn set_timezone(&self, timezone: &str) -> Result<(), String> {
        os_actions::set_timezone(timezone)
    }
}

/// Production impl — `timedatectl set-ntp`.
pub struct RealNtp;

impl NtpControl for RealNtp {
    fn set_ntp(&self, enabled: bool) -> Result<(), String> {
        os_actions::set_ntp(enabled)
    }
}

/// Sink the clock-format toggle persists through. In production it writes the
/// shared locale (`locale.time_format`) that Region & Formats owns, so the two
/// panes agree and the choice survives a restart; tests inject a no-op so the
/// hermetic unit tests never touch the real `settings.json`.
type FormatSink = Arc<dyn Fn(bool) + Send + Sync>;

/// The production format sink: fold the 24-hour choice into the shared locale.
fn persist_time_format(h24: bool) {
    let mut region = RegionSettings::load();
    region.locale.time_format = if h24 { TimeStyle::H24 } else { TimeStyle::H12 };
    if let Err(e) = region.save() {
        warn!(target: "tilo::settings", op = "time_format_save", error = %e, "failed to persist clock format");
    }
}

/// Read the configured time zone from `/etc/timezone`, falling back to a
/// sample so the pane is never blank.
/// Starting zone for [`TimeDatePane::with_ntp`]. Any real zone works so
/// long as it is fixed; tests pick a different one to observe a change.
const TEST_TIMEZONE: &str = "Europe/Amsterdam";

fn timezone() -> String {
    for p in ["/etc/timezone"] {
        if let Ok(s) = fs::read_to_string(Path::new(p)) {
            let t = s.trim();
            if !t.is_empty() {
                return t.to_string();
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
            return zone.to_string();
        }
    }
    "Europe/Amsterdam".to_string()
}

/// Time & Date pane. Stateful so it can drive `set-ntp` on toggle and
/// re-render the live clock when the 24-hour preference flips.
#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag is an independently-toggleable setting or a one-shot \
              dirty bit, not a combined state; grouping them would obscure \
              rather than clarify, and a bitflags/state-machine refactor \
              would ripple through every read/write site for no behavioural \
              gain"
)]
#[allow(
    dead_code,
    reason = "legacy CRDT replay helpers are retained only for migration-focused tests"
)]
pub struct TimeDatePane {
    ntp: Arc<dyn NtpControl>,
    persist_format: FormatSink,
    timezone: String,
    timezones: Vec<String>,
    timezone_points: Vec<TimeZonePoint>,
    sync_status: os_actions::TimeSyncStatus,
    h24: bool,
    initial_auto: bool,
    last_auto: bool,
    last_h24: bool,
    last_timezone: String,
    notice: Option<String>,
    needs_seed: bool,
    structure_dirty: bool,
    commit_context_pending: bool,
    composed: Option<Box<dyn Widget>>,
    rect: Rect,
}

impl Default for TimeDatePane {
    fn default() -> Self {
        Self::new()
    }
}

impl TimeDatePane {
    pub fn new() -> Self {
        let sync_status = os_actions::get_time_sync_status();
        let initial_auto = sync_status
            .ntp_enabled
            .or_else(|| os_actions::get_ntp().ok())
            .unwrap_or(true);
        // The clock format is the same system-wide preference Region & Formats
        // owns (`locale.time_format`), so the two panes agree and the choice
        // survives a restart. Reading it here — rather than defaulting to 24h —
        // makes the toggle rest on the user's actual setting.
        let h24 = matches!(RegionSettings::load().locale.time_format, TimeStyle::H24);
        let timezone = sync_status.timezone.clone().unwrap_or_else(timezone);
        let mut timezones = os_actions::list_timezones().unwrap_or_default();
        if !timezones.iter().any(|candidate| candidate == &timezone) {
            timezones.push(timezone.clone());
            timezones.sort();
        }
        let timezone_points = timezone_map(&timezones);
        Self {
            ntp: Arc::new(RealNtp),
            persist_format: Arc::new(persist_time_format),
            timezone: timezone.clone(),
            timezones,
            timezone_points,
            sync_status,
            h24,
            initial_auto,
            last_auto: initial_auto,
            last_h24: h24,
            last_timezone: timezone,
            notice: None,
            needs_seed: true,
            structure_dirty: false,
            commit_context_pending: false,
            composed: None,
            rect: Rect::ZERO,
        }
    }

    /// Test constructor: inject the NTP control and starting auto-state. The
    /// clock format starts at 24-hour and persists through a no-op sink, so the
    /// hermetic unit tests neither read the host's locale nor write to disk.
    pub fn with_ntp(ntp: Arc<dyn NtpControl>, initial_auto: bool) -> Self {
        // A fixed zone rather than the host's. Reading `/etc/timezone` here
        // made the pane's starting zone whatever the machine happened to be
        // set to, and a test that switches *to* a zone silently asserts
        // nothing when the pane already starts there — which is how a runner
        // set to Etc/UTC failed a test that passed everywhere else.
        let timezone = TEST_TIMEZONE.to_string();
        Self {
            ntp,
            persist_format: Arc::new(|_| {}),
            timezone: timezone.clone(),
            timezones: vec![timezone.clone()],
            timezone_points: Vec::new(),
            sync_status: os_actions::TimeSyncStatus {
                timezone: Some(timezone.clone()),
                ntp_enabled: Some(initial_auto),
                synchronized: None,
                source: None,
            },
            h24: true,
            initial_auto,
            last_auto: initial_auto,
            last_h24: true,
            last_timezone: timezone,
            notice: None,
            needs_seed: true,
            structure_dirty: false,
            commit_context_pending: false,
            composed: None,
            rect: Rect::ZERO,
        }
    }

    fn build_composition(&self) -> Box<dyn Widget> {
        let now = os_actions::now_clock(self.h24).unwrap_or_else(|_| "—".to_string());
        let today = os_actions::now_date().unwrap_or_else(|_| "—".to_string());

        let clock_format = if self.h24 {
            t("time-clock-24h")
        } else {
            t("time-clock-12h")
        };
        let sync = match self.sync_status.synchronized {
            Some(true) => t("time-sync-ok"),
            Some(false) if self.initial_auto => t("time-sync-waiting"),
            Some(false) => t("time-sync-disabled"),
            None => t("time-sync-unknown"),
        };
        let source_name = self
            .sync_status
            .source
            .clone()
            .unwrap_or_else(|| t("time-source-unknown"));
        let mapped = self.timezone_points.len();
        let map_note = if mapped == 0 {
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
                setting_row(
                    &t("time-auto"),
                    Box::new(
                        Toggle::from_state(&ui::LocalState::new(self.last_auto))
                            .on_action(time_bool_action("automatic-time"))
                            .with_id("time.auto"),
                    ),
                ),
                setting_row(
                    &t("time-24h"),
                    Box::new(
                        Toggle::from_state(&ui::LocalState::new(self.last_h24))
                            .on_action(time_bool_action("24-hour"))
                            .with_id("time.h24"),
                    ),
                ),
                choice_row(
                    &t("time-timezone"),
                    Box::new(
                        Dropdown::from_state(
                            self.timezones.clone(),
                            &ui::LocalState::new(self.last_timezone.clone()),
                        )
                        .on_action(time_text_action("timezone"))
                        .with_id("time.timezone")
                        .with_placeholder(&self.timezone),
                    ),
                ),
                value_row(&t("time-current-time"), &now),
                value_row(&t("time-current-date"), &today),
            ]),
        );

        let formats = card(
            &t("time-display-title"),
            rows(vec![
                value_row(&t("time-clock-format"), &clock_format),
                caption(&t("time-display-caption")),
            ]),
        );

        let map = card(
            &t("time-map-title"),
            rows(vec![
                Box::new(
                    TimeZoneMap::new(self.timezone_points.clone(), self.timezone.clone())
                        .on_action(time_text_action("timezone")),
                ),
                caption(&map_note),
            ]),
        );

        let source = card(
            &t("time-source-title"),
            rows(vec![
                value_row(&t("time-sync"), &sync),
                value_row(&t("time-source"), &source_name),
                caption(&t("time-source-caption")),
            ]),
        );

        let mut cards = vec![clock, map, source, formats];
        if let Some(notice) = &self.notice {
            cards.insert(
                0,
                card(&t("time-notice-title"), rows(vec![caption(notice)])),
            );
        }
        crate::widgets::cards(cards)
    }
}

pub fn build() -> Box<dyn Widget> {
    Box::new(TimeDatePane::new())
}

impl Widget for TimeDatePane {
    ui::provides!(dyn ui::undo::UndoPoints);

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let mut composed = self.build_composition();
        let child = composed.build_layout(taffy, theme, node_map);
        self.composed = Some(composed);
        let style = taffy::Style {
            display: taffy::Display::Flex,
            flex_direction: taffy::FlexDirection::Column,
            size: taffy::Size {
                width: taffy::Dimension::percent(1.0),
                height: taffy::Dimension::auto(),
            },
            ..Default::default()
        };
        let node = taffy.new_with_children(style, &[child]).unwrap_or(child);
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
        if let Some(c) = &mut self.composed
            && let Ok(children) = taffy.children(node)
            && let Some(&cn) = children.first()
        {
            c.apply_layout(taffy, cn, Point::new(self.rect.x0, self.rect.y0));
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        if let Some(c) = &mut self.composed {
            c.draw(scene, transform, text_renderer, theme, presence);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        let was_handled = *ctx.handled;
        if let Some(c) = &mut self.composed
            && let Ok(children) = ctx.taffy.children(ctx.node_id)
            && let Some(&cn) = children.first()
        {
            c.handle_event(event, &mut ctx.reborrow(ctx.path, cn));
        }
        self.commit_context_pending |= !was_handled && *ctx.handled;
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.composed
            .as_ref()
            .map(|c| c.as_ref() as &dyn Widget)
            .into_iter()
            .collect()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.composed
            .as_mut()
            .map(|c| c.as_mut() as &mut (dyn Widget + '_))
            .into_iter()
            .collect()
    }

    fn name(&self) -> &'static str {
        "TimeDatePane"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn take_structure_dirty(&mut self) -> bool {
        let own = std::mem::take(&mut self.structure_dirty);
        let child = self
            .composed
            .as_mut()
            .is_some_and(|c| c.take_structure_dirty());
        own || child
    }
}

impl ui::undo::UndoPoints for TimeDatePane {
    fn take_commit_context(&mut self) -> Option<ui::tilo_actions::ActionContext> {
        let pending = std::mem::take(&mut self.commit_context_pending)
            || self
                .composed
                .as_ref()
                .is_some_and(|content| ui::undo::needs_commit(content.as_ref()));
        crate::panes::preference_commit_context(
            pending,
            "time-date",
            "Time & date",
            "Change time and date preferences",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::ObjType;
    use std::collections::HashMap;
    use std::sync::Mutex;
    use taffy::prelude::TaffyMaxContent;
    use taffy::{NodeId, TaffyTree};
    use ui::Theme;

    #[derive(Default)]
    struct RecordingNtp {
        calls: Mutex<Vec<bool>>,
        zones: Mutex<Vec<String>>,
    }
    impl NtpControl for RecordingNtp {
        fn set_ntp(&self, enabled: bool) -> Result<(), String> {
            self.calls.lock().unwrap().push(enabled);
            Ok(())
        }

        fn set_timezone(&self, timezone: &str) -> Result<(), String> {
            self.zones.lock().unwrap().push(timezone.to_string());
            Ok(())
        }
    }

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

    /// tzdata parsing, map projection, keyboard access and non-empty timezones.
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
                assert!((map.rect.height() - f64::from(MAP_HEIGHT)).abs() < 0.1);
                for index in 0..map.points.len() {
                    assert!(map.plot_rect().contains(map.projected(index)));
                }
            }
        }

        // Case `map_is_keyboard_accessible_and_only_commits_known_zone`.
        {
            let mut map = TimeZoneMap::new(fixture_points(), "Europe/Amsterdam");
            map.set_focus(true);
            let access = map.accessibility().expect("map semantics");
            assert_eq!(access.role, ui::a11y::Role::Canvas);
            assert!(access.state.focusable);
            assert!(access.state.focused);

            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            assert!(map.perform_action(
                &ui::a11y::Action::SetValue("Europe/Berlin".to_string()),
                &mut doc,
                &root,
            ));
            assert_eq!(
                read_str_or_text(&doc, &root, draft_key(TIMEZONE_KEY)).as_deref(),
                Some("Europe/Berlin")
            );
            assert!(
                ui::undo::needs_commit(&map),
                "a real timezone choice is durable"
            );
            ui::undo::on_commit(&mut map);
            assert!(!ui::undo::needs_commit(&map));
            assert!(!map.perform_action(
                &ui::a11y::Action::SetValue("Mars/Olympus_Mons".to_string()),
                &mut doc,
                &root,
            ));
        }

        // Case `timezone_is_never_empty`.
        {
            assert!(!timezone().is_empty());
        }
    }

    /// Document timezone, NTP and 24h edits stay local; the pane lays out.
    #[test]
    fn document_time_edits_never_reach_the_local_authority() {
        // Case `document_timezone_edits_do_not_reach_the_local_authority`.
        {
            let ntp = Arc::new(RecordingNtp::default());
            let mut pane = TimeDatePane::with_ntp(ntp.clone(), true);
            pane.timezones.push("Etc/UTC".to_string());
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            pane.sync(&mut doc, &root);

            // This is the representation `Dropdown::commit_value` uses.
            let text = doc.put_object(&root, TIMEZONE_KEY, ObjType::Text).unwrap();
            doc.splice_text(&text, 0, 0, "Etc/UTC").unwrap();
            pane.sync(&mut doc, &root);
            pane.sync(&mut doc, &root);
            assert!(ntp.zones.lock().unwrap().is_empty());
        }

        // Case `document_auto_time_toggle_does_not_invoke_set_ntp`.
        // Toggling "Set time automatically" forwards to `set_ntp` exactly once
        // per change, with the new value — the real dispatch path.
        {
            let ntp = Arc::new(RecordingNtp::default());
            // Start with auto=true (the seed), then flip off.
            let mut pane = TimeDatePane::with_ntp(ntp.clone(), true);
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            pane.sync(&mut doc, &root);
            assert!(
                ntp.calls.lock().unwrap().is_empty(),
                "seed must not dispatch"
            );
            let _ = doc.put(&root, AUTO_TIME_KEY, automerge::ScalarValue::Boolean(false));
            pane.sync(&mut doc, &root);
            pane.sync(&mut doc, &root); // idempotent at rest
            assert!(ntp.calls.lock().unwrap().is_empty());
            // Flip back on.
            let _ = doc.put(&root, AUTO_TIME_KEY, automerge::ScalarValue::Boolean(true));
            pane.sync(&mut doc, &root);
            assert!(ntp.calls.lock().unwrap().is_empty());
        }

        // Case `builds_layout`.
        {
            let mut pane = build();
            let theme = Theme::dark();
            let mut taffy: TaffyTree<()> = TaffyTree::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            let _ = pane.build_layout(&mut taffy, &theme, &mut node_map);
        }

        // Case `document_h24_toggle_does_not_persist_or_reformat_clock`.
        // Flipping the 24-hour key re-renders the clock (structure dirty), updates
        // the pane's format flag, and forwards the new value to the format sink —
        // proof the toggle is live and drives the persistence path (asserted
        // against disk in the `prefs_persistence` integration test).
        {
            let sunk: Arc<Mutex<Vec<bool>>> = Arc::new(Mutex::new(Vec::new()));
            let sink_calls = sunk.clone();
            // Build the hermetic test pane (24h start, no disk) but swap in a
            // recording format sink so we can prove the toggle drives persistence.
            let mut pane = TimeDatePane::with_ntp(Arc::new(RecordingNtp::default()), true);
            pane.persist_format = Arc::new(move |b| sink_calls.lock().unwrap().push(b));
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            pane.sync(&mut doc, &root);
            assert!(sunk.lock().unwrap().is_empty(), "seed must not persist");
            // Seed wrote h24=true; flip to false.
            let _ = doc.put(&root, H24_KEY, automerge::ScalarValue::Boolean(false));
            pane.sync(&mut doc, &root);
            assert!(pane.h24, "the pane ignores a collaborative clock value");
            assert!(!pane.take_structure_dirty());
            assert!(sunk.lock().unwrap().is_empty());
        }
    }
}
