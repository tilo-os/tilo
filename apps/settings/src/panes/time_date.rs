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
use std::sync::{Arc, Mutex, PoisonError};

use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use ui::LocalState;
use ui::framework::{EffectAnswer, EffectCx, Effected};
use ui::theme::Theme;
use ui::tilo_resource;
use ui::widget::{AppModel, Dropdown, ModelPath, Toggle, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Circle, Line, Point, Rect, RoundedRect, Stroke};
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::effects::{SettingsEffect, effect};
use crate::host::{Followed, PaneCx};
use crate::i18n::{t, t_args};
use crate::region::{Locale, TimeStyle};
use crate::resource::Settings;
use crate::settings_app::SettingsApp;
use crate::widgets::{caption, card, choice_row, error_row, rows, setting_row, value_row};
use ui::i18n::ArgValue;

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
        }
    }

    fn on_action(mut self, binding: Option<tilo_actions::ActionBinding<String>>) -> Self {
        self.action = binding.map(ui::widget::ActionEmitter::new);
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
        #[allow(
            clippy::unwrap_used,
            reason = "taffy 0.10's new_leaf always inserts and returns Ok; see ui::widget::TaffyTreeExt's doc comment"
        )]
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::length(
                        theme.tokens.metrics.panel_width_utility * 11.0 / 20.0,
                    ),
                },
                flex_shrink: 0.0,
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = ui::widget::layout_rect(taffy, node, origin);
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
        // so they read as one surface's ruling rather than a second border.
        let grid_color = theme.tokens.colors.border_standard;
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
                    theme.tokens.colors.text_primary
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
    fn now(&self, h24: bool) -> (Result<String, String>, Result<String, String>);
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
        let mut status = os_actions::get_time_sync_status()?;
        if status.timezone.is_none() {
            status.timezone = timezone();
        }
        Ok(status)
    }

    fn ntp(&self) -> Result<bool, String> {
        os_actions::get_ntp()
    }

    fn zones(&self) -> Result<Vec<String>, String> {
        os_actions::list_timezones()
    }

    fn now(&self, h24: bool) -> (Result<String, String>, Result<String, String>) {
        (os_actions::now_clock(h24), os_actions::now_date())
    }

    fn set_ntp(&self, enabled: bool) -> Result<(), String> {
        os_actions::set_ntp(enabled)
    }

    fn set_timezone(&self, timezone: &str) -> Result<(), String> {
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
        Ok(self
            .status
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone())
    }

    fn ntp(&self) -> Result<bool, String> {
        Ok(self
            .status
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .ntp_enabled
            .unwrap_or(true))
    }

    fn zones(&self) -> Result<Vec<String>, String> {
        Ok([
            "America/New_York",
            "Asia/Tokyo",
            "Europe/Amsterdam",
            "Europe/Berlin",
        ]
        .map(str::to_owned)
        .to_vec())
    }

    fn now(&self, h24: bool) -> (Result<String, String>, Result<String, String>) {
        let time = if h24 { "14:05" } else { "2:05 PM" };
        (
            Ok(time.to_owned()),
            Ok("Wednesday 15 March 2023".to_owned()),
        )
    }

    fn set_ntp(&self, enabled: bool) -> Result<(), String> {
        self.status
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .ntp_enabled = Some(enabled);
        Ok(())
    }

    fn set_timezone(&self, timezone: &str) -> Result<(), String> {
        self.status
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .timezone = Some(timezone.to_owned());
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
    /// Show times on a 24-hour clock, or a 12-hour one.
    #[effect(idempotent, label = "Use a 24-hour clock")]
    SetTwentyFourHourClock { on: bool },
}

/// What the Time & Date area keeps for the window: the device's clock, and
/// where its last change stands.
pub struct TimeDateArea {
    clock: Arc<dyn Clock>,
    /// Why the last change failed, for the pane to show.
    notice: LocalState<Option<String>>,
    /// Bumped by every change the clock took, so the pane reads it again.
    applied: LocalState<u64>,
    pending_reads: LocalState<u64>,
}

impl TimeDateArea {
    /// The area over this device's clock, or a stand-in for a capture.
    pub fn new(cx: &PaneCx) -> Self {
        let clock: Arc<dyn Clock> = if cx.reaches_devices() {
            Arc::new(SystemClock)
        } else {
            Arc::new(SampleClock::default())
        };
        Self {
            clock,
            notice: LocalState::new(None),
            applied: LocalState::new(0),
            pending_reads: LocalState::new(0),
        }
    }

    pub(crate) fn statuses(&self) -> Vec<ui::framework::EffectStatus> {
        if self.pending_reads.get() == 0 {
            return Vec::new();
        }
        [
            TimeDateEffect::SET_AUTOMATIC_TIME.id,
            TimeDateEffect::SET_TIME_ZONE.id,
        ]
        .into_iter()
        .map(|id| {
            ui::framework::EffectStatus::unavailable(
                tilo_resource::ActionRef::<SettingsEffect, ()>::new(id, &[]),
                "pending_clock_read",
                t("time-reading"),
            )
        })
        .collect()
    }
}

/// Answer `effect`. Device changes run off the UI thread: timedated may ask
/// for authorisation.
pub(crate) fn answer(
    app: &SettingsApp,
    effect: TimeDateEffect,
    cx: &mut EffectCx<Settings, SettingsApp>,
) -> EffectAnswer {
    if let TimeDateEffect::SetTwentyFourHourClock { on } = effect {
        let style = if on { TimeStyle::H24 } else { TimeStyle::H12 };
        return app
            .host()
            .update(|locale: &mut Locale| locale.time_format = style);
    }
    if app.time_date().pending_reads.get() != 0 {
        return Err(t("time-reading"));
    }
    let clock = Arc::clone(&app.time_date().clock);
    Ok(cx.thread(
        move |_| match effect {
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
            TimeDateEffect::SetTwentyFourHourClock { .. } => Ok(()),
        },
        |app: &mut SettingsApp, outcome: Result<(), String>, _| {
            let area = app.time_date();
            area.applied.set(area.applied.get().wrapping_add(1));
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

type ClockText = (Result<String, String>, Result<String, String>);

#[derive(Clone, Copy)]
struct ClockRequest {
    applied: u64,
    h24: bool,
    full: bool,
}

struct ClockDetails {
    status: os_actions::TimeSyncStatus,
    status_error: Option<String>,
    auto_error: Option<String>,
    zones: Vec<String>,
    zones_error: Option<String>,
    points: Vec<TimeZonePoint>,
}

impl ClockDetails {
    fn read(clock: &dyn Clock) -> Self {
        let (mut zones, zones_error) = match clock.zones() {
            Ok(zones) => (zones, None),
            Err(error) => (Vec::new(), Some(error)),
        };
        let (mut status, status_error) = match clock.status() {
            Ok(status) => (status, None),
            Err(error) => (os_actions::TimeSyncStatus::default(), Some(error)),
        };
        let auto_error = if status.ntp_enabled.is_none() {
            match clock.ntp() {
                Ok(on) => {
                    status.ntp_enabled = Some(on);
                    None
                }
                Err(error) => Some(error),
            }
        } else {
            None
        };
        if let Some(zone) = &status.timezone
            && !zones.contains(zone)
        {
            zones.push(zone.clone());
            zones.sort();
        }
        let points = timezone_map(&zones);
        Self {
            status,
            status_error,
            auto_error,
            zones,
            zones_error,
            points,
        }
    }

    fn failed(error: String) -> Self {
        Self {
            status: os_actions::TimeSyncStatus::default(),
            status_error: Some(error.clone()),
            auto_error: Some(error.clone()),
            zones: Vec::new(),
            zones_error: Some(error),
            points: Vec::new(),
        }
    }
}

struct ClockRead {
    details: Option<ClockDetails>,
    text: ClockText,
}

impl ClockRead {
    fn failed(error: String, full: bool) -> Self {
        Self {
            details: full.then(|| ClockDetails::failed(error.clone())),
            text: (Err(error.clone()), Err(error)),
        }
    }
}

struct PendingClockRead {
    request: ClockRequest,
    receiver: std::sync::mpsc::Receiver<ClockRead>,
}

// Current time is a sampled device reading, not work triggered by hover/layout.
const CLOCK_SAMPLE_INTERVAL: ui::time::Duration = ui::time::Duration::from_secs(1);

/// Time & Date pane: what the device's clock reports, and the person's
/// clock format.
#[derive(Clone, Copy, PartialEq, Eq)]
enum CompositionState {
    Dirty,
    Retained,
}

pub struct TimeDatePane {
    cx: PaneCx,
    clock: Arc<dyn Clock>,
    locale: Followed<Locale>,
    notice: LocalState<Option<String>>,
    notice_shown: Option<String>,
    applied: LocalState<u64>,
    applied_seen: u64,
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
    clock_format: TimeStyle,
    clock_text: Option<ClockText>,
    reading: Option<PendingClockRead>,
    pending_reads: LocalState<u64>,
    next_sample: ui::time::Instant,
    composition: CompositionState,
    catalog_revision: u64,
    structure_dirty: bool,
    composed: ui::widget::Composed,
    rect: Rect,
}

impl TimeDatePane {
    /// The pane over `area`'s clock and `cx`'s locale. Every part the clock
    /// could not read is kept as its failure and shown as one, never as a
    /// default.
    pub fn new(cx: PaneCx, area: &TimeDateArea) -> Self {
        let locale = cx.follow::<Locale>();
        let h24 = matches!(locale.get().time_format, TimeStyle::H24);
        let mut pane = Self {
            h24,
            clock_format: locale.get().time_format,
            clock_text: None,
            reading: None,
            pending_reads: area.pending_reads.clone(),
            next_sample: ui::time::now(),
            cx,
            clock: Arc::clone(&area.clock),
            locale,
            notice: area.notice.clone(),
            notice_shown: area.notice.get(),
            applied: area.applied.clone(),
            applied_seen: area.applied.get(),
            timezone: String::new(),
            timezones: Vec::new(),
            timezone_points: Vec::new(),
            sync_status: os_actions::TimeSyncStatus::default(),
            status_error: None,
            timezones_error: None,
            timezone_unknown: true,
            auto_error: None,
            composition: CompositionState::Dirty,
            catalog_revision: ui::i18n::I18nManager::revision(),
            structure_dirty: false,
            composed: ui::widget::Composed::default(),
            rect: Rect::ZERO,
        };
        pane.start_read(true);
        pane
    }

    fn full_pending(&self) -> bool {
        self.clock_text.is_none()
            || self.applied_seen != self.applied.get()
            || matches!(self.clock_format, TimeStyle::H24) != self.h24
            || self.reading.as_ref().is_some_and(|read| read.request.full)
    }

    fn release_read(&mut self) {
        if self.reading.take().is_some_and(|read| read.request.full) {
            self.pending_reads
                .set(self.pending_reads.get().saturating_sub(1));
        }
    }

    fn start_read(&mut self, full: bool) {
        if full {
            self.composition = CompositionState::Dirty;
            self.structure_dirty = true;
        }
        let request = ClockRequest {
            applied: self.applied.get(),
            h24: self.h24,
            full,
        };
        let clock = Arc::clone(&self.clock);
        let (sender, receiver) = std::sync::mpsc::channel();
        let wake = ui::WakeHandle::new();
        match ui::background::Background::current().spawn("clock_snapshot", move || {
            let details = request.full.then(|| ClockDetails::read(clock.as_ref()));
            let read = ClockRead {
                details,
                text: clock.now(request.h24),
            };
            if sender.send(read).is_ok() {
                wake.wake();
            }
        }) {
            Ok(()) => {
                if full {
                    self.pending_reads
                        .set(self.pending_reads.get().saturating_add(1));
                }
                self.reading = Some(PendingClockRead { request, receiver });
            }
            Err(error) => self.adopt_read(request, ClockRead::failed(error.to_string(), full)),
        }
    }

    fn adopt_read(&mut self, request: ClockRequest, read: ClockRead) {
        if request.applied != self.applied.get() || request.h24 != self.h24 {
            return;
        }
        let changed = read.details.is_some() || self.clock_text.as_ref() != Some(&read.text);
        if let Some(details) = read.details {
            self.sync_status = details.status;
            self.status_error = details.status_error;
            self.auto_error = details.auto_error;
            self.timezone_unknown = self.sync_status.timezone.is_none();
            self.timezone = self.sync_status.timezone.clone().unwrap_or_default();
            self.timezones = details.zones;
            self.timezones_error = details.zones_error;
            self.timezone_points = details.points;
            self.applied_seen = request.applied;
            self.clock_format = if request.h24 {
                TimeStyle::H24
            } else {
                TimeStyle::H12
            };
        }
        self.clock_text = Some(read.text);
        self.next_sample = ui::time::now() + CLOCK_SAMPLE_INTERVAL;
        if changed {
            self.composition = CompositionState::Dirty;
            self.structure_dirty = true;
        }
    }

    fn poll_read(&mut self) {
        let result = self
            .reading
            .as_ref()
            .and_then(|read| match read.receiver.try_recv() {
                Ok(result) => Some((read.request, result)),
                Err(std::sync::mpsc::TryRecvError::Empty) => None,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => Some((
                    read.request,
                    ClockRead::failed(t("time-read-disconnected"), read.request.full),
                )),
            });
        if let Some((request, result)) = result {
            self.release_read();
            self.adopt_read(request, result);
        }
        if self.reading.is_none() {
            if self.full_pending() {
                self.start_read(true);
            } else if ui::time::now() >= self.next_sample {
                self.start_read(false);
            }
        }
    }

    /// The automatic-time switch, or why its position is unknown.
    fn auto_row(&self) -> Box<dyn Widget> {
        match (&self.auto_error, self.sync_status.ntp_enabled) {
            (Some(error), _) => error_row(
                &t("time-auto"),
                &t_args("time-status-error", &[("error", error.as_str().into())]),
            ),
            (None, automatic) => setting_row(
                &t("time-auto"),
                Box::new(
                    self.cx.bind(
                        Toggle::from_state(&LocalState::new(automatic.unwrap_or(false)))
                            .with_id("time.auto"),
                        effect(TimeDateEffect::SET_AUTOMATIC_TIME),
                    ),
                ),
            ),
        }
    }

    /// The zone chooser, or why there is nothing to choose from.
    fn zone_row(&self) -> Box<dyn Widget> {
        match &self.timezones_error {
            Some(error) => error_row(
                &t("time-timezone"),
                &t_args("time-zones-error", &[("error", error.as_str().into())]),
            ),
            None => choice_row(
                &t("time-timezone"),
                Box::new(
                    self.cx.bind(
                        Dropdown::from_state(
                            self.timezones.clone(),
                            &LocalState::new(self.timezone.clone()),
                        )
                        .with_id("time.timezone")
                        .with_placeholder(&if self.timezone_unknown {
                            t("time-zone-unread")
                        } else {
                            self.timezone.clone()
                        }),
                        effect(TimeDateEffect::SET_TIME_ZONE),
                    ),
                ),
            ),
        }
    }

    fn clock_rows(&self) -> (Box<dyn Widget>, Box<dyn Widget>) {
        let clock_row = |label: &str, read: Result<String, String>| match read {
            Ok(value) => value_row(label, &value),
            Err(error) => error_row(
                label,
                &t_args("time-clock-error", &[("error", error.into())]),
            ),
        };
        let (now, today) = self
            .clock_text
            .clone()
            .unwrap_or_else(|| (Err(t("time-reading")), Err(t("time-reading"))));
        let now = clock_row(&t("time-current-time"), now);
        let today = clock_row(&t("time-current-date"), today);
        (now, today)
    }

    fn build_composition(&self) -> Box<dyn Widget> {
        if self.full_pending() {
            return crate::widgets::cards(vec![card(
                &t("time-clock-title"),
                rows(vec![caption(&t("time-reading"))]),
            )]);
        }
        let (now, today) = self.clock_rows();
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
                auto_row,
                setting_row(
                    &t("time-24h"),
                    Box::new(self.cx.bind(
                        Toggle::from_state(&LocalState::new(self.h24)).with_id("time.h24"),
                        effect(TimeDateEffect::SET_TWENTY_FOUR_HOUR_CLOCK),
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
                caption(&t("time-display-caption")),
            ]),
        );

        let map = card(
            &t("time-map-title"),
            rows(vec![
                Box::new(
                    TimeZoneMap::new(self.timezone_points.clone(), self.timezone.clone())
                        .on_action(self.cx.binding(effect(TimeDateEffect::SET_TIME_ZONE))),
                ),
                caption(&map_note),
            ]),
        );

        let source = card(
            &t("time-source-title"),
            rows(match &self.status_error {
                Some(error) => vec![
                    error_row(
                        &t("time-sync"),
                        &t_args("time-status-error", &[("error", error.as_str().into())]),
                    ),
                    caption(&t("time-source-caption")),
                ],
                None => vec![
                    value_row(&t("time-sync"), &sync),
                    value_row(&t("time-source"), &source_name),
                    caption(&t("time-source-caption")),
                ],
            }),
        );

        let mut cards = vec![clock, map, source, formats];
        if let Some(notice) = &self.notice_shown {
            cards.insert(
                0,
                card(&t("time-notice-title"), rows(vec![caption(notice)])),
            );
        }
        crate::widgets::cards(cards)
    }
}

impl Widget for TimeDatePane {
    fn sync(&mut self, state: &AppModel, root: &ModelPath) {
        let notice = self.notice.get();
        if notice != self.notice_shown {
            self.notice_shown = notice;
            self.composition = CompositionState::Dirty;
            self.structure_dirty = true;
        }
        if self.locale.refresh() {
            self.h24 = matches!(self.locale.get().time_format, TimeStyle::H24);
            self.composition = CompositionState::Dirty;
            self.structure_dirty = true;
        }
        self.poll_read();
        self.composed.sync(state, root);
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        if self.composition == CompositionState::Dirty
            || self.catalog_revision != ui::i18n::I18nManager::revision()
            || self.composed.element().is_none()
        {
            let composition = self.build_composition();
            self.composed.replace(composition);
        }
        self.composition = CompositionState::Retained;
        // Building the composition can initialize its embedded catalogue.
        self.catalog_revision = ui::i18n::I18nManager::revision();
        let child = if let Some(composition) = self.composed.element_mut() {
            composition.build_layout(taffy, theme, node_map)
        } else {
            // A missing retained child needs a real composition and layout,
            // rather than an empty node or a guessed geometry.
            let composition = self.build_composition();
            self.composed
                .build_layout(composition, taffy, theme, node_map)
        };
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
        self.rect = ui::widget::layout_rect(taffy, node, origin);
        if let Some(c) = self.composed.element_mut()
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
        if let Some(c) = self.composed.element_mut() {
            c.draw(scene, transform, text_renderer, theme, presence);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if let Some(c) = self.composed.element_mut()
            && let Ok(children) = ctx.taffy.children(ctx.node_id)
            && let Some(&cn) = children.first()
        {
            c.handle_event(event, &mut ctx.reborrow(ctx.path, cn));
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.composed.children()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.composed.children_mut()
    }

    fn next_wake_deadline(&self) -> Option<ui::time::Instant> {
        self.reading.is_none().then_some(self.next_sample)
    }

    fn has_pending_visual_work(&self) -> bool {
        self.reading.is_some()
    }

    fn name(&self) -> &'static str {
        "TimeDatePane"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn take_structure_dirty(&mut self) -> bool {
        let own = std::mem::take(&mut self.structure_dirty);
        let child = self.composed.take_structure_dirty();
        own || child
    }
}

impl Drop for TimeDatePane {
    fn drop(&mut self) {
        self.release_read();
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
                assert!(
                    (map.rect.height()
                        - f64::from(
                            Theme::dark().tokens.metrics.panel_width_utility * 11.0 / 20.0
                        ))
                    .abs()
                        < 0.1
                );
                for index in 0..map.points.len() {
                    assert!(map.plot_rect().contains(map.projected(index)));
                }
            }
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
                &mut ui::widget::Edits::lend(&mut doc),
                &root,
            ));
            assert_eq!(map.selected, "Europe/Berlin");
            assert!(!map.perform_action(
                &ui::a11y::Action::SetValue("Mars/Olympus_Mons".to_string()),
                &mut ui::widget::Edits::lend(&mut doc),
                &root,
            ));
        }
    }

    struct HeldClock {
        sample: SampleClock,
        stage: &'static str,
        release: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
        entered: std::sync::mpsc::Sender<std::thread::ThreadId>,
        sampled: std::sync::atomic::AtomicUsize,
    }

    impl HeldClock {
        fn wait(&self, stage: &str) -> Result<(), String> {
            if self.stage == stage
                && let Some(release) = self.release.lock().unwrap().take()
            {
                self.entered
                    .send(std::thread::current().id())
                    .map_err(|error| error.to_string())?;
                release
                    .recv_timeout(std::time::Duration::from_secs(3))
                    .map_err(|error| error.to_string())?;
            }
            Ok(())
        }
    }

    impl Clock for HeldClock {
        fn status(&self) -> Result<os_actions::TimeSyncStatus, String> {
            self.sample.status()
        }
        fn ntp(&self) -> Result<bool, String> {
            self.sample.ntp()
        }
        fn zones(&self) -> Result<Vec<String>, String> {
            self.wait("zones")?;
            self.sample.zones()
        }
        fn now(&self, h24: bool) -> (Result<String, String>, Result<String, String>) {
            self.sampled
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            match self.wait("now") {
                Ok(()) => self.sample.now(h24),
                Err(error) => (Err(error.clone()), Err(error)),
            }
        }
        fn set_ntp(&self, on: bool) -> Result<(), String> {
            self.sample.set_ntp(on)
        }
        fn set_timezone(&self, zone: &str) -> Result<(), String> {
            self.sample.set_timezone(zone)
        }
    }

    struct HeldClockFixture {
        clock: Arc<HeldClock>,
        entry: std::sync::mpsc::Receiver<std::thread::ThreadId>,
        release: std::sync::mpsc::Sender<()>,
    }

    fn held_clock(stage: &'static str) -> HeldClockFixture {
        let (entered, entry) = std::sync::mpsc::channel();
        let (release, released) = std::sync::mpsc::channel();
        HeldClockFixture {
            clock: Arc::new(HeldClock {
                sample: SampleClock::default(),
                stage,
                release: Mutex::new(Some(released)),
                entered,
                sampled: std::sync::atomic::AtomicUsize::new(0),
            }),
            entry,
            release,
        }
    }

    fn release_clock(
        entry: std::sync::mpsc::Receiver<std::thread::ThreadId>,
        release: std::sync::mpsc::Sender<()>,
        required: bool,
    ) -> std::thread::JoinHandle<Option<std::thread::ThreadId>> {
        std::thread::spawn(move || {
            let reader = entry
                .recv_timeout(std::time::Duration::from_millis(if required {
                    3000
                } else {
                    500
                }))
                .ok();
            if required {
                assert!(
                    reader.is_some(),
                    "actual clock read must reach the held backend"
                );
            }
            if reader.is_some() {
                std::thread::sleep(std::time::Duration::from_millis(300));
                release.send(()).unwrap();
            }
            reader
        })
    }

    #[test]
    fn opening_time_date_does_not_wait_for_the_timedated_read() {
        let profile = tempfile::tempdir().unwrap();
        let cx = PaneCx::in_file(profile.path().join("settings.json"));
        let HeldClockFixture {
            clock,
            entry,
            release,
        } = held_clock("zones");
        let release = release_clock(entry, release, true);
        let area = TimeDateArea {
            clock,
            notice: LocalState::new(None),
            applied: LocalState::new(0),
            pending_reads: LocalState::new(0),
        };
        let started = std::time::Instant::now();
        let _pane = TimeDatePane::new(cx, &area);
        let elapsed = started.elapsed();
        let reader = release.join().unwrap();
        assert!(
            elapsed < std::time::Duration::from_millis(150),
            "opening the actual pane waited for timedated: {elapsed:?}, reader={reader:?}"
        );
        assert_ne!(reader, Some(std::thread::current().id()));
    }

    #[test]
    fn mounted_time_date_frames_do_not_wait_for_or_repeat_date_commands() {
        let profile = tempfile::tempdir().unwrap();
        let cx = PaneCx::in_file(profile.path().join("settings.json"));
        let HeldClockFixture {
            clock,
            entry,
            release,
        } = held_clock("now");
        // Warm the actual mounted tree before arming the next backend read;
        // the bound measures a retained frame, not first-use text setup.
        let gate = clock.release.lock().unwrap().take().unwrap();
        let area = TimeDateArea {
            clock: clock.clone(),
            notice: LocalState::new(None),
            applied: LocalState::new(0),
            pending_reads: LocalState::new(0),
        };
        let pane = TimeDatePane::new(cx, &area);
        let mut harness = ui::headless::FrameHarness::for_root(
            ui::State::new(),
            pane,
            (1200, 700),
            Theme::dark(),
        );
        assert!(harness.wait_until(ui::time::Duration::from_secs(3), |h| {
            !h.root::<TimeDatePane>().has_pending_visual_work()
        }));
        let before = clock.sampled.load(std::sync::atomic::Ordering::SeqCst);
        *clock.release.lock().unwrap() = Some(gate);
        let release = release_clock(entry, release, false);
        let started = std::time::Instant::now();
        harness
            .relayout_themed((1160, 700), Theme::light())
            .unwrap();
        let elapsed = started.elapsed();
        let after = clock.sampled.load(std::sync::atomic::Ordering::SeqCst);
        let reader = release.join().unwrap();
        assert!(
            elapsed < std::time::Duration::from_millis(150),
            "an actual retained frame waited for date: {elapsed:?}, reader={reader:?}"
        );
        assert_eq!(
            after, before,
            "retained frames cannot spawn more clock commands"
        );
        assert!(
            reader.is_none(),
            "the retained frame must use its observed clock snapshot"
        );
    }

    #[test]
    fn retained_time_date_layout_preserves_the_open_timezone_selector() {
        let mut app = TestApp::<SettingsShell>::new().unwrap();
        app.effect(&SettingsEffect::from(
            crate::effects::NavigationEffect::OpenPane {
                pane: "time_date".into(),
            },
        ))
        .unwrap();
        let harness = app.harness();
        assert!(harness.wait_until(ui::time::Duration::from_secs(3), |h| {
            h.inspection_json()
                .is_ok_and(|text| text.contains(SAMPLE_TIMEZONE))
        }));
        harness.click_named("time.timezone").unwrap();
        let open = ui::headless::SemanticExpectation::named("time.timezone").expanded(true);
        harness.expect(&open).unwrap();
        for theme in [Theme::light(), Theme::dark(), Theme::high_contrast()] {
            harness.relayout_themed((1160, 1000), theme).unwrap();
            harness.expect(&open).unwrap();
            assert!(harness.inspection_json().unwrap().contains(SAMPLE_TIMEZONE));
        }
    }

    #[test]
    fn pending_clock_read_refuses_device_jobs_but_preserves_locale_actions() {
        let mut app = TestApp::<SettingsShell>::new().unwrap();
        let HeldClockFixture {
            clock,
            entry,
            release,
        } = held_clock("zones");
        app.surface::<SettingsApp, _>(|app| app.time_date_mut().clock = clock.clone())
            .unwrap();
        app.effect(&SettingsEffect::from(
            crate::effects::NavigationEffect::OpenPane {
                pane: "time_date".into(),
            },
        ))
        .unwrap();
        let reader = entry
            .recv_timeout(std::time::Duration::from_secs(3))
            .unwrap();
        assert_ne!(reader, std::thread::current().id());
        assert_eq!(
            app.surface::<SettingsApp, _>(|app| app.time_date().statuses().len()),
            Some(2)
        );
        assert!(
            app.effect(&SettingsEffect::from(TimeDateEffect::SetAutomaticTime {
                on: false
            }))
            .is_err()
        );
        app.effect(&SettingsEffect::from(
            TimeDateEffect::SetTwentyFourHourClock { on: false },
        ))
        .unwrap();
        release.send(()).unwrap();
        assert!(
            app.harness()
                .wait_until(ui::time::Duration::from_secs(3), |h| {
                    h.inspection_json()
                        .is_ok_and(|text| text.contains("2:05 PM"))
                })
        );
        assert_eq!(app.settings::<Locale>().get().time_format, TimeStyle::H12);
        assert_eq!(
            app.surface::<SettingsApp, _>(|app| app.time_date().statuses().len()),
            Some(0)
        );
        assert!(
            clock.sample.ntp().unwrap(),
            "the refused job cannot change the device clock"
        );
        assert!(
            clock.sampled.load(std::sync::atomic::Ordering::SeqCst) >= 2,
            "the stale format reply requires a new actual sample"
        );
    }

    #[test]
    fn clock_worker_wakes_the_owner_and_drop_retires_only_its_pending_read() {
        let mut scope = ui::WakeScope::new();
        let (woke, wakes) = std::sync::mpsc::channel();
        scope
            .bind(move |_| {
                let _ = woke.send(());
            })
            .unwrap();
        let _scope = scope.enter();
        let profile = tempfile::tempdir().unwrap();
        let cx = PaneCx::in_file(profile.path().join("settings.json"));
        let HeldClockFixture {
            clock,
            entry,
            release,
        } = held_clock("zones");
        let area = TimeDateArea {
            clock,
            notice: LocalState::new(None),
            applied: LocalState::new(0),
            pending_reads: LocalState::new(0),
        };
        let pane = TimeDatePane::new(cx.clone(), &area);
        assert_ne!(
            entry
                .recv_timeout(std::time::Duration::from_secs(3))
                .unwrap(),
            std::thread::current().id()
        );
        assert_eq!(area.statuses().len(), 2);
        release.send(()).unwrap();
        wakes
            .recv_timeout(std::time::Duration::from_secs(3))
            .unwrap();
        let mut harness = ui::headless::FrameHarness::for_root(
            ui::State::new(),
            pane,
            (1200, 700),
            Theme::dark(),
        );
        assert!(harness.wait_until(ui::time::Duration::from_secs(3), |h| {
            !h.root::<TimeDatePane>().has_pending_visual_work()
        }));
        assert!(harness.inspection_json().unwrap().contains(SAMPLE_TIMEZONE));
        assert!(area.statuses().is_empty());
        let pending = TimeDatePane::new(cx, &area);
        assert_eq!(area.statuses().len(), 2);
        drop(pending);
        assert!(
            area.statuses().is_empty(),
            "a retired pane cannot keep device jobs unavailable"
        );
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
        fn now(&self, _: bool) -> (Result<String, String>, Result<String, String>) {
            (Ok("14:05".into()), Ok("Wednesday 15 March 2023".into()))
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
        let area = TimeDateArea {
            clock: Arc::new(UnreadableClock),
            notice: LocalState::new(None),
            applied: LocalState::new(0),
            pending_reads: LocalState::new(0),
        };
        let pane = TimeDatePane::new(cx, &area);
        let mut harness = ui::headless::FrameHarness::for_root(
            ui::State::new(),
            pane,
            (1200, 700),
            Theme::dark(),
        );
        assert!(harness.wait_until(ui::time::Duration::from_secs(3), |h| {
            !h.root::<TimeDatePane>().has_pending_visual_work()
        }));
        let pane = harness.root::<TimeDatePane>();
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
        assert!(
            app.harness()
                .wait_until(ui::time::Duration::from_secs(3), |h| h
                    .inspection_json()
                    .is_ok_and(|text| text.contains("Europe/Berlin")))
        );
        let frame = app.harness().inspection_json().unwrap();
        assert!(frame.contains("Europe/Berlin"), "{frame}");
        assert!(
            app.effect(&SettingsEffect::from(TimeDateEffect::SetTimeZone {
                zone: "Mars/Olympus_Mons".into(),
            }))
            .is_err()
        );

        app.effect(&SettingsEffect::from(
            TimeDateEffect::SetTwentyFourHourClock { on: false },
        ))
        .expect("the clock format is the locale's");
        assert_eq!(app.settings::<Locale>().get().time_format, TimeStyle::H12);
        assert!(
            app.harness()
                .wait_until(ui::time::Duration::from_secs(3), |h| h
                    .inspection_json()
                    .is_ok_and(|text| text.contains("2:05 PM")))
        );
        let frame = app.harness().inspection_json().unwrap();
        assert!(frame.contains("2:05 PM"), "{frame}");
    }
}
