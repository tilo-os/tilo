//! The forecast (detail) pane: what [`Core`] paints for the selected place,
//! the geometry it is laid out from, and the input it answers.
//!
//! The pane is one scrolling column on the window ground, sections separated
//! by space and a `Heading` each, never boxed (a statistic is not a card):
//!
//! - a notice when the forecast on screen is out of date (`Banner`),
//! - the hero: the condition glyph, the temperature as the screen's one
//!   `Display` number, the condition, the day's range and the place, with
//!   the sun's arc beside it when there is room,
//! - the current readings as stat tiles,
//! - the next 24 hours as a scrolling strip ([`crate::chart`]),
//! - the 7-day forecast as rows,
//! - air quality as stat tiles.
//!
//! The glyphs, the strip, the arc and the temperature bars are content
//! drawing; every size and gap is a measured role line or a theme token.

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{BlendMode, Brush, Fill, Gradient};
use winit::event::WindowEvent;

use ui::a11y::{AccessNode, Role};
use ui::charts::{BarGauge, GaugeStyle, TrendRange};
use ui::paint::PaintCtx;
use ui::text_render::{TextRenderer, TextStyle};
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::widget::{
    Banner, BannerContent, BannerKind, EmptyState, EmptyStateContent, PressOutcome, ScrollMetrics,
    StatTileFace, Tone, Trend, card_column_width, card_columns,
};

use super::{Core, ForecastNotice, SunArc, scroll_track, wheel_dx, wheel_dy, wheel_step};
use crate::api;
use crate::chart;
use crate::icon;
use crate::model::{AirQuality, DayForecast, SavedLocation, WeatherSnapshot};
use crate::units::{self, Units};
use crate::wxcode::Sky;

impl Core {
    /// Semantic summary of the currently painted forecast. This mirrors the
    /// hero's key reading while leaving the long forecast visually
    /// scannable and avoids forcing assistive technology through decorative
    /// glyphs and chart marks.
    pub fn detail_accessibility(&self, rect: Rect) -> AccessNode {
        let node = AccessNode::new(Role::Group)
            .with_name("Forecast")
            .with_bounds(rect);
        let Some(location) = self.model.selected_location() else {
            return node.with_description(if self.model.locations.is_empty() {
                "No places yet. Add a place to see its forecast."
            } else {
                "Choose a saved place to see its forecast."
            });
        };
        let detail = self.model.snapshot_for(location.id).map_or_else(
            || "Current conditions are loading.".to_string(),
            |snapshot| {
                let temp = snapshot.temp_in(snapshot.current.temperature, self.units).round();
                let feels = snapshot.temp_in(snapshot.current.apparent, self.units).round();
                format!(
                    "{temp:.0}°, {}, feels like {feels:.0}°. {} hourly readings and {} daily forecasts are available.",
                    Sky::label(snapshot.current.weather_code),
                    snapshot.hourly.len(),
                    snapshot.daily.len()
                )
            },
        );
        node.with_name(format!("{} forecast", location.name))
            .with_description(detail)
            .with_keyed_action_routes("weather-detail")
    }

    pub fn draw_detail(
        &mut self,
        rect: Rect,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        self.detail_rect = rect;
        self.body_rect = rect;
        self.wheel_step = wheel_step(theme);

        let ctx = &mut PaintCtx::new(scene, t, tr, theme);

        let Some(loc) = self.model.selected_location().cloned() else {
            self.max_scroll = 0.0;
            self.hourly_max_scroll = 0.0;
            self.hourly_viewport = Rect::ZERO;
            let title = if self.model.locations.is_empty() {
                "Add a place to see its forecast"
            } else {
                "Choose a place to see its forecast"
            };
            let glyph = |ctx: &mut PaintCtx<'_>, r: Rect| {
                icon::draw_icon(
                    ctx.scene,
                    ctx.transform,
                    Sky::PartlyCloudy,
                    true,
                    r,
                    ctx.theme,
                );
            };
            EmptyState::paint(
                ctx,
                rect,
                &EmptyStateContent {
                    illustration: Some(&glyph),
                    ..EmptyStateContent::new(title)
                },
                self.last_mouse,
            );
            return;
        };

        let gutter = f64::from(theme.tokens.spacing.s6);
        let width = (rect.width() - 2.0 * gutter).max(1.0);
        let snap = self.model.selected_snapshot().cloned();
        let aq = self.model.selected_air_quality().cloned();
        let page = DetailPage::new(ctx, width, &loc, snap.as_ref(), aq.as_ref(), self);

        let view_h = rect.height();
        self.body_content_h = page.height + 2.0 * gutter;
        self.max_scroll = (self.body_content_h - view_h).max(0.0);
        self.scroll = self.scroll.clamp(0.0, self.max_scroll);

        ctx.scene
            .push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &rect);
        let origin = Point::new(rect.x0 + gutter, rect.y0 + gutter - self.scroll);
        self.paint_page(ctx, &page, origin);
        ctx.scene.pop_layer();

        self.body_track = scroll_track(rect, theme);
        let metrics = ScrollMetrics::new(self.body_content_h, view_h, self.scroll);
        if metrics.overflows() {
            self.body_bar
                .draw(ctx.scene, t, self.body_track, metrics, theme, false);
        }
    }

    /// Paint every section of `page` from `origin` down.
    fn paint_page(&mut self, ctx: &mut PaintCtx<'_>, page: &DetailPage<'_>, origin: Point) {
        let w = page.width;
        let mut y = origin.y;
        let x = origin.x;
        let place = |h: f64, y: &mut f64| {
            let r = Rect::new(x, *y, x + w, *y + h);
            *y += h + page.section_gap;
            r
        };
        if let Some(notice) = &page.notice {
            let r = place(notice.height, &mut y);
            Banner::paint(ctx, r, &notice.content(), self.last_mouse);
        }
        let Some(snap) = page.snapshot else {
            ctx.text_role(
                "Loading the forecast",
                TextRole::Body,
                ctx.theme.text_dim,
                Point::new(x, y),
                None,
            );
            return;
        };
        let r = place(page.hero.height, &mut y);
        page.hero.paint(ctx, r);
        let r = place(page.readings.height, &mut y);
        page.readings.paint(ctx, r);
        let r = place(page.hourly_h, &mut y);
        self.paint_hourly(ctx, r, snap, page);
        let r = place(page.days.height, &mut y);
        page.days.paint(ctx, r, page.header_h + page.header_gap);
        let r = place(page.air.height, &mut y);
        page.air.paint(ctx, r, page.header_h + page.header_gap);
    }

    /// The next 24 hours: the section heading, then the strip in its band
    /// with the shared horizontal scroll bar under it.
    fn paint_hourly(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        r: Rect,
        snap: &WeatherSnapshot,
        page: &DetailPage<'_>,
    ) {
        let theme = ctx.theme;
        section_heading(ctx, "Next 24 hours", r.origin());
        let band_top = r.y0 + page.header_h + page.header_gap;
        let band = Rect::new(r.x0, band_top, r.x1, band_top + page.strip.height);
        let cols: Vec<chart::HourColumn> = snap
            .hourly
            .iter()
            .enumerate()
            .map(|(i, h)| chart::HourColumn {
                hour: h.hour_label(),
                temp: snap.temp_in(h.temperature, self.units),
                precip: h.precip_probability,
                code: h.weather_code,
                is_day: h.is_day,
                is_now: i == 0,
            })
            .collect();
        // Each hour gets at least the strip's minimum column; the strip
        // scrolls when the 24 columns are wider than the band rather than
        // squeezing them.
        let n = cols.len().max(1);
        let col_w = (band.width() / n as f64).max(page.min_hour_w);
        let content_w = col_w * n as f64;
        self.hourly_viewport = band;
        self.hourly_content_w = content_w;
        self.hourly_max_scroll = (content_w - band.width()).max(0.0);
        self.hourly_scroll_x = self.hourly_scroll_x.clamp(0.0, self.hourly_max_scroll);

        chart::HourlyStrip {
            cols: &cols,
            col_w,
            scroll_x: self.hourly_scroll_x,
            temp_suffix: "°",
        }
        .draw(ctx, band);

        let s1 = f64::from(theme.tokens.spacing.s1);
        let track_top = band.y1 + page.header_gap;
        self.hourly_track = Rect::new(band.x0, track_top, band.x1, track_top + s1);
        let metrics = self.hourly_metrics();
        if metrics.overflows() {
            self.hourly_bar.draw(
                ctx.scene,
                ctx.transform,
                self.hourly_track,
                metrics,
                theme,
                false,
            );
        }
    }

    /// Whether the strip's shared scrollbar is mid-drag (for pointer capture).
    pub fn hourly_scrollbar_dragging(&self) -> bool {
        self.hourly_bar.is_dragging()
    }

    fn hourly_metrics(&self) -> ScrollMetrics {
        ScrollMetrics::new(
            self.hourly_content_w,
            self.hourly_viewport.width(),
            self.hourly_scroll_x,
        )
    }

    fn body_metrics(&self) -> ScrollMetrics {
        ScrollMetrics::new(self.body_content_h, self.body_rect.height(), self.scroll)
    }

    /// Handle an event routed to the detail pane. Returns true if consumed.
    pub fn handle_detail_event(&mut self, event: &WindowEvent, scale_factor: f64) -> bool {
        use winit::event::{ElementState, MouseButton};

        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_mouse = Point::new(position.x / scale_factor, position.y / scale_factor);
        }
        // While the city list holds the input (a place being added, its
        // menu, a drag), the forecast stays out of the way.
        if self.overview_owns_input() {
            return false;
        }
        match event {
            WindowEvent::CursorMoved { .. } => {
                // A live scrollbar drag tracks the cursor.
                if let Some(off) = self.hourly_bar.drag_to(
                    self.hourly_track,
                    self.hourly_metrics(),
                    self.last_mouse,
                ) {
                    self.hourly_scroll_x = off;
                    return true;
                }
                if let Some(off) =
                    self.body_bar
                        .drag_to(self.body_track, self.body_metrics(), self.last_mouse)
                {
                    self.scroll = off;
                    return true;
                }
                self.hourly_bar.update_hover(
                    self.hourly_track,
                    self.hourly_metrics(),
                    self.last_mouse,
                );
                self.body_bar
                    .update_hover(self.body_track, self.body_metrics(), self.last_mouse);
                false
            }
            WindowEvent::MouseWheel { delta, .. } => self.handle_detail_wheel(*delta),
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => self.handle_detail_press(),
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                let a = self.hourly_bar.release();
                let b = self.body_bar.release();
                a || b
            }
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                self.handle_detail_key(&event.logical_key)
            }
            _ => false,
        }
    }

    fn handle_detail_wheel(&mut self, delta: winit::event::MouseScrollDelta) -> bool {
        let dy = wheel_dy(delta, self.wheel_step);
        let dx = wheel_dx(delta, self.wheel_step);
        // Over the strip: scroll it horizontally. A horizontal delta maps
        // directly; a vertical wheel nudges it too, falling through to the
        // page scroll once it reaches an end.
        if self.hourly_viewport.contains(self.last_mouse) && self.hourly_max_scroll > 0.0 {
            let want = if dx.abs() > f64::EPSILON { -dx } else { -dy };
            let before = self.hourly_scroll_x;
            self.hourly_scroll_x = (self.hourly_scroll_x + want).clamp(0.0, self.hourly_max_scroll);
            if (self.hourly_scroll_x - before).abs() > f64::EPSILON {
                return true;
            }
        }
        if self.detail_rect.contains(self.last_mouse) && self.max_scroll > 0.0 {
            self.scroll = (self.scroll - dy).clamp(0.0, self.max_scroll);
            return true;
        }
        false
    }

    fn handle_detail_press(&mut self) -> bool {
        for (bar, track, metrics, horizontal) in [
            (
                &mut self.hourly_bar,
                self.hourly_track,
                ScrollMetrics::new(
                    self.hourly_content_w,
                    self.hourly_viewport.width(),
                    self.hourly_scroll_x,
                ),
                true,
            ),
            (
                &mut self.body_bar,
                self.body_track,
                ScrollMetrics::new(self.body_content_h, self.body_rect.height(), self.scroll),
                false,
            ),
        ] {
            if !metrics.overflows() {
                continue;
            }
            match bar.press(track, metrics, self.last_mouse) {
                PressOutcome::Thumb => return true,
                PressOutcome::Page(off) => {
                    if horizontal {
                        self.hourly_scroll_x = off;
                    } else {
                        self.scroll = off;
                    }
                    return true;
                }
                PressOutcome::Miss => {}
            }
        }
        false
    }

    fn handle_detail_key(&mut self, key: &winit::keyboard::Key) -> bool {
        use winit::keyboard::{Key, NamedKey};

        let page = (self.hourly_viewport.width() * 0.8).max(1.0);
        match key {
            Key::Named(NamedKey::ArrowLeft) if self.hourly_max_scroll > 0.0 => {
                self.hourly_scroll_x = (self.hourly_scroll_x - page).max(0.0);
                true
            }
            Key::Named(NamedKey::ArrowRight) if self.hourly_max_scroll > 0.0 => {
                self.hourly_scroll_x = (self.hourly_scroll_x + page).min(self.hourly_max_scroll);
                true
            }
            Key::Named(NamedKey::Home) if self.hourly_max_scroll > 0.0 => {
                self.hourly_scroll_x = 0.0;
                true
            }
            Key::Named(NamedKey::End) if self.hourly_max_scroll > 0.0 => {
                self.hourly_scroll_x = self.hourly_max_scroll;
                true
            }
            Key::Character(text) if text.as_str().eq_ignore_ascii_case("r") => {
                self.refresh_selected();
                true
            }
            _ => false,
        }
    }

    pub fn detail_tooltip(&self) -> Option<&'static str> {
        (self.hourly_viewport.contains(self.last_mouse) && self.hourly_max_scroll > 0.0)
            .then_some("Use Left and Right Arrow to scroll the hours")
    }

    /// Park the forecast scrolled to the bottom for a short-window shot.
    pub fn seed_scrolled(&mut self) {
        self.scroll = f64::MAX;
    }
}

// =====================================================================
// The page: every section measured once per frame
// =====================================================================

/// The forecast page for one paint: every section's content and height,
/// measured once so the scroll extent and the painters agree.
struct DetailPage<'a> {
    width: f64,
    section_gap: f64,
    header_gap: f64,
    /// One `Heading` line: every section's title row.
    header_h: f64,
    notice: Option<Notice>,
    snapshot: Option<&'a WeatherSnapshot>,
    hero: Hero,
    readings: Tiles,
    strip: chart::StripRows,
    min_hour_w: f64,
    hourly_h: f64,
    days: Days<'a>,
    air: Air,
    height: f64,
}

impl<'a> DetailPage<'a> {
    fn new(
        ctx: &mut PaintCtx<'_>,
        width: f64,
        loc: &SavedLocation,
        snapshot: Option<&'a WeatherSnapshot>,
        aq: Option<&AirQuality>,
        core: &Core,
    ) -> Self {
        let theme = ctx.theme;
        let sp = &theme.tokens.spacing;
        let section_gap = f64::from(sp.s8);
        let header_gap = f64::from(sp.s3);
        let header_h = ctx.line_box(TextRole::Heading);
        let notice = core.notice.map(|kind| Notice::new(ctx, kind, width));
        let units = core.units;
        let hero = Hero::new(ctx, width, loc, snapshot, units);
        let readings = Tiles::readings(ctx, width, snapshot, units, !hero.has_arc());
        let strip = chart::strip_rows(theme, ctx.text);
        let min_hour_w = chart::min_column_width(theme, ctx.text);
        let hourly_h = header_h + header_gap + strip.height + header_gap + f64::from(sp.s1);
        let days = Days::new(ctx, width, snapshot, units, header_h + header_gap);
        let air = Air::new(ctx, width, aq, core.is_fetching(), header_h + header_gap);
        let mut parts = Vec::new();
        if let Some(n) = &notice {
            parts.push(n.height);
        }
        if snapshot.is_some() {
            parts.extend([
                hero.height,
                readings.height,
                hourly_h,
                days.height,
                air.height,
            ]);
        } else {
            parts.push(ctx.line_box(TextRole::Body));
        }
        let height = parts.iter().sum::<f64>() + section_gap * parts.len().saturating_sub(1) as f64;
        Self {
            width,
            section_gap,
            header_gap,
            header_h,
            notice,
            snapshot,
            hero,
            readings,
            strip,
            min_hour_w,
            hourly_h,
            days,
            air,
            height,
        }
    }
}

/// A section's title, in `Heading`, sentence case.
fn section_heading(ctx: &mut PaintCtx<'_>, title: &str, at: Point) {
    ctx.text_role(title, TextRole::Heading, ctx.theme.text_primary, at, None);
}

/// The lasting problem with the forecast on screen, above it.
struct Notice {
    kind: ForecastNotice,
    height: f64,
}

impl Notice {
    fn new(ctx: &mut PaintCtx<'_>, kind: ForecastNotice, width: f64) -> Self {
        let mut notice = Self { kind, height: 0.0 };
        notice.height = banner_height(ctx, &notice.content(), width);
        notice
    }

    fn content(&self) -> BannerContent<'static> {
        match self.kind {
            ForecastNotice::UpdateFailed => {
                BannerContent::new(BannerKind::Error, "Couldn't update the forecast")
                    .with_message(Some("Check your connection, then refresh."))
            }
            ForecastNotice::Offline => BannerContent::new(BannerKind::Warning, "You're offline")
                .with_message(Some("This is the last forecast saved on this device.")),
        }
    }
}

/// The height a banner with `content` takes at `width`: its title and
/// message lines with the banner's padding, never below a panel row.
fn banner_height(ctx: &mut PaintCtx<'_>, content: &BannerContent<'_>, width: f64) -> f64 {
    let theme = ctx.theme;
    let t = &theme.tokens;
    let pad_x = f64::from(t.spacing.s4);
    let pad_y = f64::from(t.spacing.s3);
    let text_w = (width - 2.0 * pad_x - f64::from(t.metrics.icon_md) - f64::from(t.spacing.s3))
        .max(1.0) as f32;
    let title = ctx
        .measure_role(content.title, TextRole::Body, Some(text_w))
        .height;
    let message = content.message.map_or(0.0, |m| {
        ctx.measure_role(m, TextRole::Caption, Some(text_w)).height + f64::from(t.spacing.s1)
    });
    (title + message + 2.0 * pad_y)
        .max(f64::from(t.metrics.panel_row_height))
        .ceil()
}

// ---- hero -------------------------------------------------------------

/// The hero: the condition glyph, the temperature, the condition, the
/// day's range and the place; the sun's arc beside it on a wide pane.
struct Hero {
    sky: Option<(Sky, bool)>,
    temperature: String,
    condition: String,
    range: String,
    place: String,
    sun: Option<SunArc>,
    glyph: f64,
    height: f64,
}

impl Hero {
    fn new(
        ctx: &mut PaintCtx<'_>,
        width: f64,
        loc: &SavedLocation,
        snapshot: Option<&WeatherSnapshot>,
        units: Units,
    ) -> Self {
        let theme = ctx.theme;
        let sp = &theme.tokens.spacing;
        let s1 = f64::from(sp.s1);
        let glyph = f64::from(sp.s16);
        let (sky, temperature, condition, range) = snapshot.map_or_else(
            || (None, String::new(), String::new(), String::new()),
            |snap| {
                let cur = &snap.current;
                let range = snap.daily.first().map_or_else(String::new, |today| {
                    format!(
                        "High {}°, low {}°",
                        snap.temp_in(today.temp_max, units).round() as i64,
                        snap.temp_in(today.temp_min, units).round() as i64
                    )
                });
                (
                    Some((Sky::from_code(cur.weather_code), cur.is_day)),
                    format!("{}°", snap.temp_in(cur.temperature, units).round() as i64),
                    Sky::label(cur.weather_code).to_string(),
                    range,
                )
            },
        );
        let text_h = ctx.line_box(TextRole::Display)
            + ctx.line_box(TextRole::Heading)
            + ctx.line_box(TextRole::Body)
            + ctx.line_box(TextRole::Caption)
            + 3.0 * s1;
        // The arc stands beside the hero only where the pane leaves the
        // text its own room: wider than a detail column's minimum.
        let sun = snapshot
            .filter(|_| width >= f64::from(theme.tokens.metrics.detail_min_width) + glyph * 2.0)
            .and_then(|snap| {
                let today = snap.daily.first()?;
                Some(SunArc::new(
                    ctx,
                    &snap.current.time,
                    snap.current.is_day,
                    today,
                ))
            });
        let arc_h = sun.as_ref().map_or(0.0, |arc| arc.height);
        Self {
            sky,
            temperature,
            condition,
            range,
            place: loc.subtitle(),
            sun,
            glyph,
            height: text_h.max(glyph).max(arc_h).ceil(),
        }
    }

    fn has_arc(&self) -> bool {
        self.sun.is_some()
    }

    fn paint(&self, ctx: &mut PaintCtx<'_>, area: Rect) {
        let theme = ctx.theme;
        let sp = &theme.tokens.spacing;
        let gap = f64::from(sp.s1);
        let colors = &theme.tokens.colors;
        if let Some((sky, is_day)) = self.sky {
            let top = area.y0 + (area.height() - self.glyph) / 2.0;
            icon::draw_icon(
                ctx.scene,
                ctx.transform,
                sky,
                is_day,
                Rect::new(area.x0, top, area.x0 + self.glyph, top + self.glyph),
                theme,
            );
        }
        let left = area.x0 + self.glyph + f64::from(sp.s4);
        let mut top = area.y0;
        let display = theme.text(TextRole::Display).numeric();
        ctx.text(
            &self.temperature,
            &display,
            colors.text_primary,
            Point::new(left, top),
            None,
        );
        top += ctx.line_box(TextRole::Display) + gap;
        for (text, role, ink) in [
            (&self.condition, TextRole::Heading, colors.text_primary),
            (&self.range, TextRole::Body, colors.text_dim),
            (&self.place, TextRole::Caption, colors.text_dim),
        ] {
            ctx.text_role(text, role, ink, Point::new(left, top), None);
            top += ctx.line_box(role) + gap;
        }
        if let Some(sun) = &self.sun {
            sun.paint(
                ctx,
                Rect::new(area.x1 - sun.width, area.y0, area.x1, area.y1),
            );
        }
    }
}

// ---- stat tiles ---------------------------------------------------------

/// One stat tile's content.
struct Tile {
    label: &'static str,
    value: String,
    unit: Option<String>,
    /// A word that qualifies the value ("High", "WSW"), under it.
    note: Option<String>,
    tone: Option<Tone>,
}

impl Tile {
    fn new(label: &'static str, value: impl Into<String>) -> Self {
        Self {
            label,
            value: value.into(),
            unit: None,
            note: None,
            tone: None,
        }
    }

    fn unit(mut self, unit: impl Into<String>) -> Self {
        self.unit = Some(unit.into());
        self
    }

    fn note(mut self, note: impl Into<String>) -> Self {
        self.note = Some(note.into());
        self
    }

    fn tone(mut self, tone: Tone) -> Self {
        self.tone = Some(tone);
        self
    }

    fn face(&self) -> StatTileFace<'_> {
        let mut face = StatTileFace::new(self.label, &self.value);
        if let Some(unit) = &self.unit {
            face = face.unit(unit);
        }
        if let Some(note) = &self.note {
            face = face.change(note, Trend::Flat);
        }
        if let Some(tone) = self.tone {
            face = face.tone(tone);
        }
        face
    }
}

/// A set of stat tiles laid out like the toolkit's `StatGroup`: as many
/// equal columns as fit and divide the count evenly, rows a section apart.
struct Tiles {
    items: Vec<Tile>,
    cols: usize,
    tile_h: f64,
    column_gap: f64,
    row_gap: f64,
    width: f64,
    height: f64,
    /// Space above the tiles (a section heading), inside `height`.
    lead: f64,
}

impl Tiles {
    fn new(ctx: &mut PaintCtx<'_>, width: f64, tiles: Vec<Tile>, lead: f64) -> Self {
        let theme = ctx.theme;
        let rhythm = &theme.tokens.spacing.rhythm;
        let column_gap = f64::from(rhythm.group);
        let row_gap = f64::from(rhythm.section);
        let min_tile = (f64::from(theme.text(TextRole::Body).line_height_px()) * 7.0).round();
        let count = tiles.len().max(1);
        let fits = card_columns(width, min_tile, column_gap);
        let cols = (1..=count.min(fits))
            .rev()
            .find(|columns| count.is_multiple_of(*columns))
            .unwrap_or(1);
        let tile_h = tiles
            .iter()
            .map(|tile| tile.face().height(ctx.text, theme))
            .fold(0.0, f64::max);
        let rows = tiles.len().div_ceil(cols);
        let height = lead + rows as f64 * tile_h + rows.saturating_sub(1) as f64 * row_gap;
        Self {
            items: tiles,
            cols,
            tile_h,
            column_gap,
            row_gap,
            width,
            height,
            lead,
        }
    }

    /// The current readings: feels like, wind, humidity, precipitation,
    /// pressure and UV; sunrise and sunset too when the hero has no arc.
    fn readings(
        ctx: &mut PaintCtx<'_>,
        width: f64,
        snapshot: Option<&WeatherSnapshot>,
        units: Units,
        with_sun: bool,
    ) -> Self {
        let Some(snap) = snapshot else {
            return Self::new(ctx, width, Vec::new(), 0.0);
        };
        let cur = &snap.current;
        let mut tiles = vec![
            Tile::new(
                "Feels like",
                format!("{}°", snap.temp_in(cur.apparent, units).round() as i64),
            ),
            Tile::new(
                "Wind",
                format!("{}", snap.wind_in(cur.wind_speed, units).round() as i64),
            )
            .unit(units.wind_suffix())
            .note(units::compass(cur.wind_direction)),
            Tile::new("Humidity", format!("{}", cur.humidity.round() as i64)).unit("%"),
            Tile::new("Precipitation", format!("{:.1}", cur.precipitation)).unit("mm"),
            Tile::new("Pressure", format!("{}", cur.pressure.round() as i64)).unit("hPa"),
            Tile::new("UV index", format!("{}", cur.uv_index.round() as i64))
                .note(uv_word(cur.uv_index)),
        ];
        if with_sun && let Some(today) = snap.daily.first() {
            tiles.push(Tile::new("Sunrise", hhmm(&today.sunrise)));
            tiles.push(Tile::new("Sunset", hhmm(&today.sunset)));
        }
        Self::new(ctx, width, tiles, 0.0)
    }

    fn paint(&self, ctx: &mut PaintCtx<'_>, r: Rect) {
        let tile_w = card_column_width(self.width, self.cols, self.column_gap);
        for (i, tile) in self.items.iter().enumerate() {
            let (row, col) = (i / self.cols, i % self.cols);
            let x = r.x0 + col as f64 * (tile_w + self.column_gap);
            let y = r.y0 + self.lead + row as f64 * (self.tile_h + self.row_gap);
            tile.face()
                .paint(ctx, Rect::new(x, y, x + tile_w, y + self.tile_h));
        }
    }
}

// ---- 7-day forecast -----------------------------------------------------

/// The 7-day forecast: its heading, then one row per day.
struct Days<'a> {
    snapshot: Option<&'a WeatherSnapshot>,
    units: Units,
    columns: DayColumns,
    row_h: f64,
    height: f64,
}

impl<'a> Days<'a> {
    fn new(
        ctx: &mut PaintCtx<'_>,
        width: f64,
        snapshot: Option<&'a WeatherSnapshot>,
        units: Units,
        lead: f64,
    ) -> Self {
        let theme = ctx.theme;
        let sp = &theme.tokens.spacing;
        let columns = DayColumns::measure(ctx, snapshot, units);
        let row_h =
            (ctx.line_box(TextRole::Body).max(columns.icon_edge) + 2.0 * f64::from(sp.s2)).ceil();
        let n = snapshot.map_or(0, |s| s.daily.len());
        let _ = width;
        Self {
            snapshot,
            units,
            columns,
            row_h,
            height: lead + n as f64 * row_h,
        }
    }

    fn paint(&self, ctx: &mut PaintCtx<'_>, r: Rect, lead: f64) {
        section_heading(ctx, "7-day forecast", r.origin());
        let Some(snap) = self.snapshot else {
            return;
        };
        let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
        for d in &snap.daily {
            lo = lo.min(d.temp_min);
            hi = hi.max(d.temp_max);
        }
        if (hi - lo).abs() < 1.0 {
            hi += 1.0;
            lo -= 1.0;
        }
        let rows = ForecastRows {
            geo: DayRowGeometry::new(r.x0, r.x1, &self.columns, &ctx.theme.tokens.spacing),
            snap,
            units: self.units,
            week: TrendRange { lo, hi },
        };
        for (i, d) in snap.daily.iter().enumerate() {
            let y = r.y0 + lead + i as f64 * self.row_h;
            rows.draw_row(ctx, i, d, Rect::new(r.x0, y, r.x1, y + self.row_h));
        }
    }
}

/// The measured widths of a day row's columns: each the widest text that
/// column paints this frame, in the role it paints it in.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct DayColumns {
    pub(super) label_w: f64,
    pub(super) icon_edge: f64,
    pub(super) cond_min_w: f64,
    pub(super) precip_w: f64,
    pub(super) uv_w: f64,
    pub(super) wind_w: f64,
    pub(super) temp_w: f64,
}

impl DayColumns {
    fn measure(ctx: &mut PaintCtx<'_>, snapshot: Option<&WeatherSnapshot>, units: Units) -> Self {
        let theme = ctx.theme;
        let body = theme.text(TextRole::Body);
        let numeric = theme.text(TextRole::Body).numeric();
        let strong = theme.text(TextRole::Body).numeric().strong();
        let caption = theme.text(TextRole::Caption).numeric();
        let mut width =
            |text: &str, style: &TextStyle| f64::from(ctx.text.measure_style(text, None, style).0);
        let mut label_w = width("Tomorrow", &body);
        let mut temp_w: f64 = 0.0;
        if let Some(snap) = snapshot {
            for (i, d) in snap.daily.iter().enumerate() {
                label_w = label_w.max(width(&day_label(i, d), &body));
                let lo = snap.temp_in(d.temp_min, units).round() as i64;
                let hi = snap.temp_in(d.temp_max, units).round() as i64;
                temp_w = temp_w
                    .max(width(&format!("{lo}°"), &numeric))
                    .max(width(&format!("{hi}°"), &strong));
            }
        }
        let drop = drop_edge(theme);
        Self {
            label_w: label_w.ceil(),
            icon_edge: f64::from(theme.tokens.metrics.icon_lg) + f64::from(theme.tokens.spacing.s1),
            cond_min_w: width("Mainly clear", &body).ceil(),
            precip_w: (drop + f64::from(theme.tokens.spacing.s1) + width("100%", &caption)).ceil(),
            uv_w: width("UV 10", &caption).ceil(),
            wind_w: width(&format!("100 {}", units.wind_suffix()), &caption).ceil(),
            temp_w: temp_w.ceil(),
        }
    }
}

/// The edge of the raindrop beside a chance of precipitation.
fn drop_edge(theme: &Theme) -> f64 {
    f64::from(theme.tokens.metrics.icon_sm)
}

/// "Today", "Tomorrow", then the weekday.
fn day_label(i: usize, d: &DayForecast) -> String {
    match i {
        0 => "Today".to_string(),
        1 => "Tomorrow".to_string(),
        _ => weekday(&d.date),
    }
}

/// Precomputed x-geometry for one row of the 7-day forecast, shared by every
/// row so they align and agree on which optional columns (condition label,
/// UV/wind extras, precipitation) fit.
#[derive(Clone, Copy, Debug)]
pub(super) struct DayRowGeometry {
    pub(super) inner_l: f64,
    pub(super) inner_r: f64,
    pub(super) icon_x0: f64,
    pub(super) icon_x1: f64,
    pub(super) cond_x: f64,
    pub(super) bar_x0: f64,
    pub(super) bar_x1: f64,
    pub(super) lo_x: f64,
    pub(super) precip_left: f64,
    pub(super) extras_x: f64,
    pub(super) wind_right: f64,
    pub(super) show_precip: bool,
    pub(super) show_extras: bool,
    pub(super) show_cond: bool,
    pub(super) columns: DayColumns,
}

impl DayRowGeometry {
    /// One column grid for every row between `x0` and `x1`, with the
    /// theme's steps between neighbours: `s3` between siblings (label,
    /// glyph, extras, precipitation, temperatures), `s2` between a value and
    /// its own bar. Optional columns show only when they keep that gap.
    pub(super) fn new(
        x0: f64,
        x1: f64,
        columns: &DayColumns,
        spacing: &ui::tokens::spacing::Spacing,
    ) -> Self {
        let (s2, s3) = (f64::from(spacing.s2), f64::from(spacing.s3));
        let s16 = f64::from(spacing.s16);
        let c = *columns;
        let dw = x1 - x0;
        let bar_w = (dw * 0.22).clamp(s16, 2.0 * s16);
        let bar_x1 = x1 - s2 - c.temp_w;
        let bar_x0 = bar_x1 - bar_w;
        let lo_x = bar_x0 - s2;
        let precip_right = lo_x - c.temp_w - s3;
        let precip_left = precip_right - c.precip_w;
        let icon_x0 = x0 + c.label_w + s3;
        let icon_x1 = icon_x0 + c.icon_edge;
        let cond_x = icon_x1 + s3;
        let show_precip = precip_left >= icon_x1 + s3;
        // Per-day extras (UV-index max + wind max) take a column to the left
        // of the precipitation/low cluster, shown only when the row is wide
        // enough to fit them without crowding the condition text.
        let wind_right = if show_precip {
            precip_left - s3
        } else {
            lo_x - c.temp_w - s3
        };
        let extras_x = wind_right - c.wind_w - s3 - c.uv_w;
        let show_extras = extras_x - cond_x >= c.cond_min_w + s3;
        let cond_limit = if show_extras {
            extras_x - s3
        } else {
            wind_right
        };
        let show_cond = cond_limit - cond_x >= c.cond_min_w;
        Self {
            inner_l: x0,
            inner_r: x1,
            icon_x0,
            icon_x1,
            cond_x,
            bar_x0,
            bar_x1,
            lo_x,
            precip_left,
            extras_x,
            wind_right,
            show_precip,
            show_extras,
            show_cond,
            columns: c,
        }
    }
}

/// The 7-day row painter: the shared column geometry, the forecast and units
/// its labels read, and the week's temperature range each day's bar spans.
struct ForecastRows<'a> {
    geo: DayRowGeometry,
    snap: &'a WeatherSnapshot,
    units: Units,
    week: TrendRange,
}

impl ForecastRows<'_> {
    /// One row of the 7-day forecast: day label, condition glyph (+ label
    /// when there's room), UV/wind extras and precipitation (when there's
    /// room), and the low/high temperature bar.
    fn draw_row(&self, ctx: &mut PaintCtx<'_>, i: usize, d: &DayForecast, row: Rect) {
        let theme = ctx.theme;
        let c = &theme.tokens.colors;
        let geo = &self.geo;
        let cy = row.center().y;
        if i > 0 {
            // Rows are separated by a hairline on the ground they stand on.
            ui::surface::separator(
                ctx.scene,
                ctx.transform,
                geo.inner_l,
                geo.inner_r,
                row.y0 + 0.5,
                0.0,
                c.surfaces.content,
            );
        }
        ctx.text_role_in_band(
            &day_label(i, d),
            TextRole::Body,
            c.text_primary,
            geo.inner_l,
            row,
            None,
        );
        let edge = geo.columns.icon_edge;
        icon::draw_icon(
            ctx.scene,
            ctx.transform,
            Sky::from_code(d.weather_code),
            true,
            Rect::new(geo.icon_x0, cy - edge / 2.0, geo.icon_x1, cy + edge / 2.0),
            theme,
        );
        if geo.show_cond {
            let limit = if geo.show_extras {
                geo.extras_x - f64::from(theme.tokens.spacing.s3)
            } else {
                geo.wind_right
            };
            let style = theme.text(TextRole::Body);
            let label = ctx.text.elide_to_width_style(
                Sky::label(d.weather_code),
                (limit - geo.cond_x).max(1.0) as f32,
                &style,
            );
            ctx.text_role_in_band(&label, TextRole::Body, c.text_dim, geo.cond_x, row, None);
        }
        let caption = theme.text(TextRole::Caption).numeric();
        if geo.show_extras {
            let (uv, wind) = day_extra_labels(d, self.snap, self.units);
            let top = |ctx: &mut PaintCtx<'_>| {
                ui::text_metrics::centered_text_top(
                    row.y0,
                    row.height(),
                    ctx.line_box(TextRole::Caption),
                )
            };
            let y = top(ctx);
            ctx.text(&uv, &caption, c.text_dim, Point::new(geo.extras_x, y), None);
            let (ww, _) = ctx.text.measure_style(&wind, None, &caption);
            ctx.text(
                &wind,
                &caption,
                c.text_dim,
                Point::new(geo.wind_right - f64::from(ww), y),
                None,
            );
        }
        if geo.show_precip {
            let drop = drop_edge(theme);
            icon::draw_drop(
                ctx.scene,
                ctx.transform,
                Rect::new(
                    geo.precip_left,
                    cy - drop / 2.0,
                    geo.precip_left + drop,
                    cy + drop / 2.0,
                ),
                theme,
            );
            let text = format!("{}%", d.precip_probability.round() as i64);
            let y = ui::text_metrics::centered_text_top(
                row.y0,
                row.height(),
                ctx.line_box(TextRole::Caption),
            );
            ctx.text(
                &text,
                &caption,
                c.text_dim,
                Point::new(
                    geo.precip_left + drop + f64::from(theme.tokens.spacing.s1),
                    y,
                ),
                None,
            );
        }
        self.draw_temps(ctx, d, row);
    }

    /// The low and high readings and the day's span of the week's range
    /// between them: one cool-to-warm wash spans the whole track and each
    /// day shows its slice of it.
    fn draw_temps(&self, ctx: &mut PaintCtx<'_>, d: &DayForecast, row: Rect) {
        let theme = ctx.theme;
        let c = &theme.tokens.colors;
        let geo = &self.geo;
        let cy = row.center().y;
        let lo = self.snap.temp_in(d.temp_min, self.units).round() as i64;
        let hi = self.snap.temp_in(d.temp_max, self.units).round() as i64;
        let numeric = theme.text(TextRole::Body).numeric();
        let strong = numeric.clone().strong();
        let y =
            ui::text_metrics::centered_text_top(row.y0, row.height(), ctx.line_box(TextRole::Body));
        let lo_text = format!("{lo}°");
        let (lw, _) = ctx.text.measure_style(&lo_text, None, &numeric);
        ctx.text(
            &lo_text,
            &numeric,
            c.text_dim,
            Point::new(geo.lo_x - f64::from(lw), y),
            None,
        );
        let s1 = f64::from(theme.tokens.spacing.s1);
        let grad = Gradient::new_linear(Point::new(geo.bar_x0, cy), Point::new(geo.bar_x1, cy))
            .with_stops([
                (0.0_f32, icon::rain_colour(theme)),
                (1.0_f32, icon::sun_colour(theme)),
            ]);
        BarGauge::draw_span(
            ctx.scene,
            ctx.transform,
            Rect::new(
                geo.bar_x0,
                cy - s1 / 2.0 - s1 / 4.0,
                geo.bar_x1,
                cy + s1 / 2.0 + s1 / 4.0,
            ),
            theme,
            (
                self.week.fraction(d.temp_min) as f32,
                self.week.fraction(d.temp_max) as f32,
            ),
            &Brush::Gradient(grad),
            &GaugeStyle::default(),
        );
        ctx.text(
            &format!("{hi}°"),
            &strong,
            c.text_primary,
            Point::new(geo.bar_x1 + f64::from(theme.tokens.spacing.s2), y),
            None,
        );
    }
}

// ---- air quality --------------------------------------------------------

/// Air quality: its heading, then the indices and pollutants as stat tiles,
/// or one line saying why there are none.
struct Air {
    tiles: Option<Tiles>,
    message: &'static str,
    height: f64,
}

impl Air {
    fn new(
        ctx: &mut PaintCtx<'_>,
        width: f64,
        aq: Option<&AirQuality>,
        fetching: bool,
        lead: f64,
    ) -> Self {
        let Some(aq) = aq.filter(|a| a.has_any()) else {
            let message = if fetching {
                "Loading air quality"
            } else {
                "Air quality isn't available for this place."
            };
            let height = lead + ctx.line_box(TextRole::Body);
            return Self {
                tiles: None,
                message,
                height,
            };
        };
        let value =
            |v: Option<f64>| v.map_or_else(|| "—".to_string(), |v| format!("{}", v.round() as i64));
        let mut european = Tile::new("European AQI", value(aq.european_aqi));
        if let Some(index) = aq.european_aqi {
            european = european
                .note(api::european_aqi_band(index))
                .tone(aqi_tone(index));
        }
        let tiles = vec![
            european,
            Tile::new("US AQI", value(aq.us_aqi)),
            Tile::new("PM2.5", value(aq.pm2_5)).unit("µg/m³"),
            Tile::new("PM10", value(aq.pm10)).unit("µg/m³"),
            Tile::new("NO₂", value(aq.nitrogen_dioxide)).unit("µg/m³"),
            Tile::new("O₃", value(aq.ozone)).unit("µg/m³"),
        ];
        let tiles = Tiles::new(ctx, width, tiles, lead);
        let height = tiles.height;
        Self {
            tiles: Some(tiles),
            message: "",
            height,
        }
    }

    fn paint(&self, ctx: &mut PaintCtx<'_>, r: Rect, lead: f64) {
        section_heading(ctx, "Air quality", r.origin());
        match &self.tiles {
            Some(tiles) => tiles.paint(ctx, r),
            None => ctx.text_role(
                self.message,
                TextRole::Body,
                ctx.theme.text_dim,
                Point::new(r.x0, r.y0 + lead),
                None,
            ),
        }
    }
}

/// What a European AQI reading means for the person: good and fair air are
/// fine, moderate is worth knowing, anything worse is a warning.
fn aqi_tone(aqi: f64) -> Tone {
    match aqi.round() as i64 {
        i64::MIN..=40 => Tone::Success,
        41..=60 => Tone::Warning,
        _ => Tone::Danger,
    }
}

// =====================================================================
// Pure helpers
// =====================================================================

pub(super) fn hhmm(iso: &str) -> String {
    iso.get(11..16).unwrap_or("--:--").to_string()
}

/// Parse an "HH:MM" clock string into minutes since local midnight.
fn hhmm_to_minutes(s: &str) -> Option<i64> {
    let (h, m) = s.split_once(':')?;
    let h: i64 = h.trim().parse().ok()?;
    let m: i64 = m.trim().parse().ok()?;
    if !(0..24).contains(&h) || !(0..60).contains(&m) {
        return None;
    }
    Some(h * 60 + m)
}

/// Fraction along the daytime arc for the sun's current position: 0 at
/// sunrise (left horizon), 0.5 at solar noon (apex), 1 at sunset (right
/// horizon). Clamps to the nearest horizon before sunrise / after sunset.
/// Returns `None` when any clock string is unparseable or the day has no
/// length, so the caller can fall back rather than divide by zero.
pub(super) fn sun_arc_fraction(
    now_hhmm: &str,
    sunrise_hhmm: &str,
    sunset_hhmm: &str,
) -> Option<f64> {
    let now = hhmm_to_minutes(now_hhmm)?;
    let sr = hhmm_to_minutes(sunrise_hhmm)?;
    let ss = hhmm_to_minutes(sunset_hhmm)?;
    if ss <= sr {
        return None;
    }
    Some(((now - sr) as f64 / (ss - sr) as f64).clamp(0.0, 1.0))
}

/// The per-day "extras" labels shown at the right of a wide 7-day row: the
/// UV-index max and the wind max converted to the display units. Kept pure so
/// the exact strings the row renders are unit-testable.
fn day_extra_labels(d: &DayForecast, snap: &WeatherSnapshot, units: Units) -> (String, String) {
    let uv = d.uv_index_max.round().max(0.0) as i64;
    let wind = snap.wind_in(d.wind_max, units).round() as i64;
    (
        format!("UV {uv}"),
        format!("{wind} {}", units.wind_suffix()),
    )
}

fn weekday(date: &str) -> String {
    use chrono::NaiveDate;
    NaiveDate::parse_from_str(date, "%Y-%m-%d")
        .map_or_else(|_| date.to_string(), |d| d.format("%A").to_string())
}

fn uv_word(uv: f64) -> &'static str {
    match uv.round() as i64 {
        0..=2 => "Low",
        3..=5 => "Moderate",
        6..=7 => "High",
        8..=10 => "Very high",
        _ => "Extreme",
    }
}

#[cfg(test)]
mod day_row_geometry_tests {
    use super::{DayColumns, DayRowGeometry};
    use ui::tokens::spacing::Spacing;

    fn columns(temp_w: f64) -> DayColumns {
        DayColumns {
            label_w: 72.0,
            icon_edge: 28.0,
            cond_min_w: 80.0,
            precip_w: 46.0,
            uv_w: 36.0,
            wind_w: 56.0,
            temp_w,
        }
    }

    #[test]
    fn day_row_columns_keep_token_gaps_and_clear_the_glyph() {
        // Every neighbouring pair in a 7-day row keeps a token-step gap at
        // every width the app lays out (HIG §7 "nothing touches").
        let sp = Spacing::default();
        let (s2, s3) = (f64::from(sp.s2), f64::from(sp.s3));
        for (w, temp_w) in [
            (312.0_f64, 30.0_f64),
            (372.0, 30.0),
            (440.0, 30.0),
            (604.0, 30.0),
            (884.0, 30.0),
            (1200.0, 30.0),
            (372.0, 38.0),
            (884.0, 42.0),
        ] {
            let c = columns(temp_w);
            let geo = DayRowGeometry::new(0.0, w, &c, &sp);
            assert!(geo.icon_x0 - (geo.inner_l + c.label_w) >= s3 - 1e-9);
            assert!(
                geo.bar_x0 - geo.lo_x >= s2 - 1e-9,
                "{w}: low touches its bar"
            );
            assert!(
                geo.inner_r - (geo.bar_x1 + s2) >= temp_w - 1e-9,
                "{w}: a {temp_w}px high runs past the edge"
            );
            if geo.show_precip {
                assert!(
                    geo.precip_left - geo.icon_x1 >= s3 - 1e-9,
                    "{w}: precipitation lands on the glyph"
                );
            }
            if geo.show_extras {
                assert!(geo.extras_x - geo.cond_x >= s3);
                let next = if geo.show_precip {
                    geo.precip_left
                } else {
                    geo.lo_x - temp_w
                };
                assert!(next - geo.wind_right >= s3 - 1e-9, "{w}: wind runs on");
            }
        }
        // A tablet-width row keeps the precipitation column, clear of the glyph.
        let geo = DayRowGeometry::new(0.0, 372.0, &columns(30.0), &sp);
        assert!(geo.show_precip);
        assert!(geo.precip_left - geo.icon_x1 >= s3);
    }
}

#[cfg(test)]
mod helper_tests {
    use super::{day_extra_labels, hhmm_to_minutes, sun_arc_fraction};
    use crate::units::Units;

    #[test]
    fn sun_arc_and_day_extras_map_real_values() {
        // Sunrise → 0 (left horizon), solar noon → 0.5 (apex), sunset → 1.
        assert_eq!(sun_arc_fraction("06:00", "06:00", "18:00"), Some(0.0));
        assert_eq!(sun_arc_fraction("12:00", "06:00", "18:00"), Some(0.5));
        assert_eq!(sun_arc_fraction("18:00", "06:00", "18:00"), Some(1.0));
        assert_eq!(sun_arc_fraction("09:00", "06:00", "18:00"), Some(0.25));
        // Before sunrise pins to the left horizon, after sunset to the right.
        assert_eq!(sun_arc_fraction("03:00", "06:00", "18:00"), Some(0.0));
        assert_eq!(sun_arc_fraction("23:30", "06:00", "18:00"), Some(1.0));
        assert_eq!(sun_arc_fraction("--:--", "06:00", "18:00"), None);
        assert_eq!(sun_arc_fraction("12:00", "18:00", "06:00"), None);
        assert_eq!(sun_arc_fraction("12:00", "12:00", "12:00"), None);
        assert_eq!(hhmm_to_minutes("25:00"), None);
        assert_eq!(hhmm_to_minutes("07:30"), Some(450));

        let m = crate::sample::sample_model();
        let snap = m.selected_snapshot().unwrap(); // San Francisco, metric source
        let day0 = &snap.daily[0];
        assert!(day0.wind_max > 0.0 && day0.uv_index_max > 0.0);
        let (uv, wind) = day_extra_labels(day0, snap, Units::METRIC);
        assert_eq!(uv, "UV 6");
        assert_eq!(wind, "24 km/h");
        // Imperial converts the wind (24 km/h ≈ 15 mph); the UV index is unitless.
        let (uv_i, wind_i) = day_extra_labels(day0, snap, Units::IMPERIAL);
        assert_eq!(uv_i, "UV 6");
        assert_eq!(wind_i, "15 mph");
    }
}

#[cfg(test)]
mod page_tests {
    use super::{DetailPage, Tiles};
    use crate::core::Core;
    use crate::units::Units;
    use ui::paint::PaintCtx;
    use ui::text_render::TextRenderer;
    use ui::theme::Theme;
    use vello::Scene;
    use vello::kurbo::Affine;

    fn page_height(width: f64, scale: f32) -> (f64, usize, f64) {
        let model = crate::sample::sample_model();
        let core = Core::new(model.clone(), Units::METRIC);
        let theme = Theme::dark().with_text_scale(scale);
        let mut tr = TextRenderer::new();
        tr.set_text_scale(scale);
        let mut scene = Scene::new();
        let mut ctx = PaintCtx::new(&mut scene, Affine::IDENTITY, &mut tr, &theme);
        let loc = model.selected_location().unwrap();
        let page = DetailPage::new(
            &mut ctx,
            width,
            loc,
            model.selected_snapshot(),
            model.selected_air_quality(),
            &core,
        );
        (page.height, page.readings.cols, page.readings.tile_h)
    }

    #[test]
    fn large_text_grows_the_page_and_reflows_the_tiles() {
        let (normal, wide_cols, normal_tile) = page_height(892.0, 1.0);
        let (large, large_cols, large_tile) = page_height(892.0, 2.0);
        assert!(large > normal * 1.4, "{large} vs {normal}");
        assert!(large_tile > normal_tile);
        assert!(large_cols < wide_cols);
        // Tiles always divide evenly into their columns.
        let (_, narrow_cols, _) = page_height(380.0, 1.0);
        assert_eq!(
            8 % narrow_cols,
            0,
            "a narrow pane shows sunrise and sunset as tiles"
        );
        let _ = Tiles::readings;
    }
}
