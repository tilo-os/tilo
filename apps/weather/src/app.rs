//! Root widget for the weather app.
//!
//! Layout is done **manually** in `draw` from a single root rect (the
//! whole window): a fixed-width left rail (search + saved locations +
//! units) and a flexible main column (header, current-conditions hero,
//! metric tiles, the 24h temperature chart, and the 7-day list). Because
//! everything is positioned arithmetically from `self.rect`, a freshly
//! fetched snapshot needs no taffy rebuild — the next `draw` simply reads
//! the new model.
//!
//! ## The `--shots` / live split
//! `--shots` renders exactly one offscreen frame with no event loop and no
//! Tokio runtime, so it can't await a network fetch. The widget is
//! therefore seeded with bundled [`crate::sample`] data and renders that
//! immediately. The background [`crate::net::WeatherService`] is spawned
//! **lazily on the first real input event** — which `--shots` never
//! delivers — so a screenshot is fully populated and completely offline,
//! while the live binary fetches real Open-Meteo data and overwrites the
//! sample snapshots in place.

use std::collections::HashMap;
use std::time::Instant;

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Circle, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Brush, Color, Fill, Gradient};
use winit::event::WindowEvent;

use ui::Widget;
use ui::color_compat::ColorCompat;
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;

use crate::chart::{self, ChartPoint};
use crate::doc::WeatherDoc;
use crate::icon;
use crate::model::WeatherModel;
use crate::net::{Request, Response, WeatherService};
use crate::units::{self, TempUnit, WindUnit};
use crate::wxcode::Sky;

const RAIL_W: f64 = 300.0;
const PAD: f64 = 28.0;
const RP: f64 = 20.0; // rail inner padding

/// A click target rebuilt every `draw` and hit-tested in `handle_event`.
struct Hotspot {
    rect: Rect,
    action: Action,
}

#[derive(Clone)]
enum Action {
    SelectLocation(usize),
    RemoveLocation(usize),
    SetTemp(TempUnit),
    SetWind(WindUnit),
    FocusSearch,
    PickResult(usize),
}

pub struct WeatherApp {
    model: WeatherModel,
    rect: Rect,
    root_node: Option<NodeId>,
    last_mouse: Point,
    hotspots: Vec<Hotspot>,

    // Search state (ephemeral UI — not collaborative).
    search_query: String,
    search_focused: bool,
    search_results: Vec<crate::model::SavedLocation>,
    search_seq: u64,

    // Networking (live only; None until first input event).
    service: Option<WeatherService>,
    initial_fetch_done: bool,
    inflight: usize,
    last_request_at: Instant,
    status: String,

    // Bookkeeping.
    seeded_doc: bool,
    pending_doc_write: bool,
}

impl WeatherApp {
    pub fn new() -> Self {
        Self {
            model: crate::sample::sample_model(),
            rect: Rect::ZERO,
            root_node: None,
            last_mouse: Point::ZERO,
            hotspots: Vec::new(),
            search_query: String::new(),
            search_focused: false,
            search_results: Vec::new(),
            search_seq: 0,
            service: None,
            initial_fetch_done: false,
            inflight: 0,
            last_request_at: Instant::now(),
            status: "Sample data — live updates on launch".to_string(),
            seeded_doc: false,
            pending_doc_write: false,
        }
    }

    /// Spawn the network worker (idempotent) and queue an initial forecast
    /// refresh for every saved location. Called from the first input event,
    /// so `--shots` (which delivers none) stays fully offline.
    fn ensure_service(&mut self) {
        if self.service.is_none() {
            self.service = Some(WeatherService::spawn());
        }
        if !self.initial_fetch_done {
            self.refresh_all();
            self.initial_fetch_done = true;
        }
    }

    fn refresh_all(&mut self) {
        let Some(svc) = self.service.as_ref() else { return };
        for loc in &self.model.locations {
            svc.request(Request::Forecast {
                location_id: loc.id,
                lat: loc.latitude,
                lon: loc.longitude,
                units: self.model.units,
            });
            self.inflight += 1;
        }
        self.last_request_at = Instant::now();
        self.status = "Updating…".to_string();
    }

    fn refresh_selected(&mut self) {
        let Some(svc) = self.service.as_ref() else { return };
        let Some(loc) = self.model.selected_location() else { return };
        svc.request(Request::Forecast {
            location_id: loc.id,
            lat: loc.latitude,
            lon: loc.longitude,
            units: self.model.units,
        });
        self.inflight += 1;
        self.last_request_at = Instant::now();
        self.status = "Updating…".to_string();
    }

    fn trigger_geocode(&mut self) {
        let q = self.search_query.trim().to_string();
        if q.is_empty() {
            self.search_results.clear();
            return;
        }
        self.ensure_service();
        if let Some(svc) = self.service.as_ref() {
            self.search_seq += 1;
            svc.request(Request::Geocode { seq: self.search_seq, query: q });
            self.status = "Searching…".to_string();
        }
    }

    /// Drain any ready network responses into the model. Returns true if
    /// the model changed (caller persists / redraws).
    fn poll_service(&mut self) -> bool {
        let Some(svc) = self.service.as_ref() else { return false };
        let responses = svc.drain();
        if responses.is_empty() {
            return false;
        }
        let mut changed = false;
        for resp in responses {
            match resp {
                Response::Forecast(snap) => {
                    self.inflight = self.inflight.saturating_sub(1);
                    self.model.snapshots.insert(snap.location_id, snap);
                    self.pending_doc_write = true;
                    self.status = "Updated just now".to_string();
                    changed = true;
                }
                Response::Geocode { seq, results } => {
                    if seq == self.search_seq {
                        self.search_results = results;
                        self.status = if self.search_results.is_empty() {
                            "No matches".to_string()
                        } else {
                            format!("{} matches", self.search_results.len())
                        };
                        changed = true;
                    }
                }
                Response::Error(e) => {
                    self.inflight = self.inflight.saturating_sub(1);
                    self.status = format!("Offline · {e}");
                    changed = true;
                }
            }
        }
        changed
    }

    /// Day/night accent: a touch lighter & cooler at night so the curve and
    /// highlights stay legible against the darker night background.
    fn accent(&self, theme: &Theme) -> Color {
        let day = self
            .model
            .selected_snapshot()
            .map(|s| s.current.is_day)
            .unwrap_or(true);
        if day {
            theme.tokens.colors.accent[5]
        } else {
            theme.tokens.colors.accent[6]
        }
    }

    fn is_day(&self) -> bool {
        self.model
            .selected_snapshot()
            .map(|s| s.current.is_day)
            .unwrap_or(true)
    }

    // ---- drawing -----------------------------------------------------

    fn draw_background(&self, scene: &mut Scene, t: Affine, theme: &Theme) {
        let c = &theme.tokens.colors;
        let base = c.surfaces.base;
        // Day → cool blue sky tint at the top; night → deepen toward black.
        let top = if self.is_day() {
            lerp_color(base, c.accent[6], 0.20)
        } else {
            lerp_color(base, c.neutral[0], 0.55)
        };
        let grad = Gradient::new_linear(
            Point::new(self.rect.x0, self.rect.y0),
            Point::new(self.rect.x0, self.rect.y0 + self.rect.height() * 0.65),
        )
        .with_stops([(0.0_f32, top), (1.0_f32, base)]);
        scene.fill(Fill::NonZero, t, &Brush::Gradient(grad), None, &self.rect);
    }

    fn draw_left_rail(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        hot: &mut Vec<Hotspot>,
    ) {
        let c = &theme.tokens.colors;
        let rail = Rect::new(self.rect.x0, self.rect.y0, self.rect.x0 + RAIL_W, self.rect.y1);
        // Rail panel — a translucent raised surface separating it from the
        // gradient content area.
        scene.fill(Fill::NonZero, t, c.surfaces.raised.with_alpha_factor(0.92), None, &rail);
        scene.stroke(
            &Stroke::new(1.0),
            t,
            c.border_standard,
            None,
            &Line::new(Point::new(rail.x1, rail.y0), Point::new(rail.x1, rail.y1)),
        );

        let x = rail.x0 + RP;
        let w = RAIL_W - 2.0 * RP;

        // Title.
        text_left(scene, tr, "Weather", x, rail.y0 + 24.0, 22.0, TextWeight::Bold, c.text_primary);
        text_left(scene, tr, "Open-Meteo · qOS", x, rail.y0 + 52.0, 12.0, TextWeight::Regular, c.text_dim);

        // Search field.
        let sy = rail.y0 + 78.0;
        let sh = 38.0;
        let field = RoundedRect::new(x, sy, x + w, sy + sh, theme.tokens.radii.md as f64);
        let field_bg = if self.search_focused { c.neutral[2] } else { c.surfaces.base };
        scene.fill(Fill::NonZero, t, field_bg, None, &field);
        scene.stroke(
            &Stroke::new(if self.search_focused { 1.5 } else { 1.0 }),
            t,
            if self.search_focused { c.border_focus } else { c.border_standard },
            None,
            &field,
        );
        // Magnifier glyph.
        let mg = Point::new(x + 16.0, sy + sh / 2.0);
        scene.stroke(&Stroke::new(1.6), t, c.text_dim, None, &Circle::new(mg, 5.0));
        scene.stroke(
            &Stroke::new(1.6),
            t,
            c.text_dim,
            None,
            &Line::new(Point::new(mg.x + 4.0, mg.y + 4.0), Point::new(mg.x + 9.0, mg.y + 9.0)),
        );
        let (qtext, qcolor) = if self.search_query.is_empty() {
            ("Search city…".to_string(), c.text_dim)
        } else {
            (self.search_query.clone(), c.text_primary)
        };
        text_left(scene, tr, &qtext, x + 30.0, sy + sh / 2.0 - 8.0, 13.0, TextWeight::Regular, qcolor);
        if self.search_focused {
            // Caret after the query text.
            let (tw, _) = tr.measure(&self.search_query, 13.0, None);
            let cx = x + 30.0 + tw as f64 + 1.0;
            scene.fill(
                Fill::NonZero,
                t,
                c.border_focus,
                None,
                &Rect::new(cx, sy + 10.0, cx + 1.5, sy + sh - 10.0),
            );
        }
        hot.push(Hotspot { rect: field.rect(), action: Action::FocusSearch });

        let mut y = sy + sh + 12.0;

        // Search results (live only; empty in --shots).
        if self.search_focused && !self.search_results.is_empty() {
            for (i, r) in self.search_results.iter().take(6).enumerate() {
                let rh = 34.0;
                let row = Rect::new(x, y, x + w, y + rh);
                if row.contains(self.last_mouse) {
                    scene.fill(Fill::NonZero, t, c.neutral[2], None, &RoundedRect::from_rect(row, 6.0));
                }
                text_left(scene, tr, &r.name, x + 10.0, y + 7.0, 13.0, TextWeight::Medium, c.text_primary);
                text_right(scene, tr, &r.country, x + w - 10.0, y + 9.0, 11.0, TextWeight::Regular, c.text_dim);
                hot.push(Hotspot { rect: row, action: Action::PickResult(i) });
                y += rh + 2.0;
            }
            y += 6.0;
        }

        // Saved locations.
        text_left(scene, tr, "SAVED PLACES", x, y, 10.0, TextWeight::Semibold, c.text_dim);
        y += 22.0;
        let units = self.model.units;
        for (i, loc) in self.model.locations.iter().enumerate() {
            let ch = 62.0;
            let chip = Rect::new(x, y, x + w, y + ch);
            let selected = i == self.model.selected;
            let bg = if selected {
                lerp_color(c.surfaces.base, self.accent(theme), 0.18)
            } else if chip.contains(self.last_mouse) {
                c.neutral[2]
            } else {
                c.surfaces.base
            };
            let shape = RoundedRect::from_rect(chip, theme.tokens.radii.md as f64);
            scene.fill(Fill::NonZero, t, bg, None, &shape);
            if selected {
                scene.stroke(&Stroke::new(1.5), t, self.accent(theme), None, &shape);
            }
            // Icon.
            if let Some(snap) = self.model.snapshot_for(loc.id) {
                let sky = Sky::from_code(snap.current.weather_code);
                icon::draw_icon(
                    scene,
                    t,
                    sky,
                    snap.current.is_day,
                    Rect::new(chip.x0 + 10.0, chip.y0 + 11.0, chip.x0 + 50.0, chip.y0 + 51.0),
                    theme,
                );
            }
            text_left(scene, tr, &loc.name, chip.x0 + 58.0, chip.y0 + 13.0, 15.0, TextWeight::Semibold, c.text_primary);
            text_left(scene, tr, &loc.country, chip.x0 + 58.0, chip.y0 + 35.0, 12.0, TextWeight::Regular, c.text_dim);
            if let Some(snap) = self.model.snapshot_for(loc.id) {
                let temp = snap.temp_in(snap.current.temperature, units).round() as i64;
                text_right(
                    scene,
                    tr,
                    &format!("{temp}°"),
                    chip.x1 - 14.0,
                    chip.y0 + 18.0,
                    24.0,
                    TextWeight::Bold,
                    c.text_primary,
                );
            }
            hot.push(Hotspot { rect: chip, action: Action::SelectLocation(i) });
            // Remove affordance (only when more than one place).
            if self.model.locations.len() > 1 && chip.contains(self.last_mouse) {
                let rm = Rect::new(chip.x1 - 22.0, chip.y0 + 4.0, chip.x1 - 6.0, chip.y0 + 20.0);
                let cxp = (rm.x0 + rm.x1) / 2.0;
                let cyp = (rm.y0 + rm.y1) / 2.0;
                scene.stroke(&Stroke::new(1.4), t, c.text_dim, None, &Line::new(Point::new(cxp - 4.0, cyp - 4.0), Point::new(cxp + 4.0, cyp + 4.0)));
                scene.stroke(&Stroke::new(1.4), t, c.text_dim, None, &Line::new(Point::new(cxp + 4.0, cyp - 4.0), Point::new(cxp - 4.0, cyp + 4.0)));
                hot.push(Hotspot { rect: rm, action: Action::RemoveLocation(i) });
            }
            y += ch + 8.0;
        }

        // Units + status anchored to the bottom of the rail.
        self.draw_units(scene, t, tr, theme, rail, hot);
    }

    fn draw_units(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        rail: Rect,
        hot: &mut Vec<Hotspot>,
    ) {
        let c = &theme.tokens.colors;
        let x = rail.x0 + RP;
        let w = RAIL_W - 2.0 * RP;
        let base_y = rail.y1 - 96.0;

        text_left(scene, tr, "UNITS", x, base_y - 22.0, 10.0, TextWeight::Semibold, c.text_dim);

        // Temperature segmented control.
        let seg_h = 30.0;
        let half = (w - 6.0) / 2.0;
        let temp_segs = [("°C", TempUnit::Celsius), ("°F", TempUnit::Fahrenheit)];
        for (i, (label, unit)) in temp_segs.iter().enumerate() {
            let sx = x + i as f64 * (half + 6.0);
            let seg = Rect::new(sx, base_y, sx + half, base_y + seg_h);
            let on = self.model.units.temp == *unit;
            let shape = RoundedRect::from_rect(seg, theme.tokens.radii.sm as f64);
            scene.fill(Fill::NonZero, t, if on { self.accent(theme) } else { c.surfaces.base }, None, &shape);
            if !on {
                scene.stroke(&Stroke::new(1.0), t, c.border_standard, None, &shape);
            }
            text_center(
                scene,
                tr,
                label,
                (seg.x0 + seg.x1) / 2.0,
                seg.y0 + seg_h / 2.0 - 8.0,
                13.0,
                TextWeight::Semibold,
                if on { Color::WHITE } else { c.text_dim },
            );
            hot.push(Hotspot { rect: seg, action: Action::SetTemp(*unit) });
        }

        // Wind segmented control.
        let wy = base_y + seg_h + 8.0;
        let wind_segs = [("km/h", WindUnit::Kmh), ("mph", WindUnit::Mph)];
        for (i, (label, unit)) in wind_segs.iter().enumerate() {
            let sx = x + i as f64 * (half + 6.0);
            let seg = Rect::new(sx, wy, sx + half, wy + seg_h);
            let on = self.model.units.wind == *unit;
            let shape = RoundedRect::from_rect(seg, theme.tokens.radii.sm as f64);
            scene.fill(Fill::NonZero, t, if on { self.accent(theme) } else { c.surfaces.base }, None, &shape);
            if !on {
                scene.stroke(&Stroke::new(1.0), t, c.border_standard, None, &shape);
            }
            text_center(
                scene,
                tr,
                label,
                (seg.x0 + seg.x1) / 2.0,
                seg.y0 + seg_h / 2.0 - 8.0,
                13.0,
                TextWeight::Semibold,
                if on { Color::WHITE } else { c.text_dim },
            );
            hot.push(Hotspot { rect: seg, action: Action::SetWind(*unit) });
        }

        // Status line.
        text_left(scene, tr, &self.status, x, rail.y1 - 22.0, 11.0, TextWeight::Regular, c.text_dim);
    }

    fn draw_main(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let c = &theme.tokens.colors;
        let main = Rect::new(self.rect.x0 + RAIL_W, self.rect.y0, self.rect.x1, self.rect.y1);
        let mx = main.x0 + PAD;
        let mw = main.width() - 2.0 * PAD;
        let right = main.x1 - PAD;
        let accent = self.accent(theme);

        let Some(loc) = self.model.selected_location().cloned() else {
            text_left(scene, tr, "No location selected", mx, main.y0 + PAD, 18.0, TextWeight::Medium, c.text_dim);
            return;
        };
        let snap = match self.model.selected_snapshot() {
            Some(s) => s.clone(),
            None => {
                text_left(scene, tr, "Loading…", mx, main.y0 + PAD, 18.0, TextWeight::Medium, c.text_dim);
                return;
            }
        };
        let units = self.model.units;
        let cur = &snap.current;

        // ---- Header ----
        let mut y = main.y0 + PAD;
        text_left(scene, tr, &loc.name, mx, y, 28.0, TextWeight::Bold, c.text_primary);
        text_left(scene, tr, &loc.subtitle(), mx, y + 36.0, 13.0, TextWeight::Regular, c.text_dim);
        // Right: date + time + day/night pill.
        let date_str = fmt_date(&cur.time);
        text_right(scene, tr, &date_str, right, y + 2.0, 14.0, TextWeight::Medium, c.text_primary);
        let time_str = hhmm(&cur.time);
        text_right(scene, tr, &format!("Local time {time_str}"), right, y + 26.0, 12.0, TextWeight::Regular, c.text_dim);
        // Day / night pill.
        let pill_label = if cur.is_day { "Daytime" } else { "Night" };
        let (plw, _) = tr.measure(pill_label, 11.0, None);
        let pill_w = plw as f64 + 34.0;
        let pill = Rect::new(right - pill_w, y + 44.0, right, y + 44.0 + 22.0);
        let pill_shape = RoundedRect::from_rect(pill, 11.0);
        scene.fill(Fill::NonZero, t, lerp_color(c.surfaces.base, accent, 0.25), None, &pill_shape);
        // Tiny sun/moon dot.
        let dotc = if cur.is_day { c.semantic.warning } else { c.neutral[8] };
        scene.fill(Fill::NonZero, t, dotc, None, &Circle::new(Point::new(pill.x0 + 13.0, (pill.y0 + pill.y1) / 2.0), 5.0));
        text_left(scene, tr, pill_label, pill.x0 + 24.0, (pill.y0 + pill.y1) / 2.0 - 7.0, 11.0, TextWeight::Semibold, c.text_primary);

        // Clear the stacked header (name/subtitle on the left; date/time/pill
        // on the right reach ~y+66) before the hero card starts.
        y += 78.0;

        // ---- Hero card ----
        let hero_h = 176.0;
        let hero = Rect::new(mx, y, mx + mw, y + hero_h);
        card(scene, t, hero, theme);
        // Big icon.
        let sky = Sky::from_code(cur.weather_code);
        icon::draw_icon(
            scene,
            t,
            sky,
            cur.is_day,
            Rect::new(hero.x0 + 26.0, hero.y0 + 40.0, hero.x0 + 122.0, hero.y0 + 136.0),
            theme,
        );
        // Temperature.
        let temp = snap.temp_in(cur.temperature, units).round() as i64;
        let temp_str = format!("{temp}");
        let temp_x = hero.x0 + 140.0;
        tr.draw_text_weighted(
            scene,
            &temp_str,
            76.0,
            c.text_primary,
            t * Affine::translate((temp_x, hero.y0 + 34.0)),
            None,
            TextWeight::Bold,
        );
        let (tnw, _) = tr.measure_weighted(&temp_str, 76.0, None, TextWeight::Bold);
        text_left(
            scene,
            tr,
            units.degree_glyph(),
            temp_x + tnw as f64 + 4.0,
            hero.y0 + 44.0,
            34.0,
            TextWeight::Regular,
            c.text_dim,
        );
        // Condition + feels-like.
        let cond = Sky::label(cur.weather_code);
        text_left(scene, tr, cond, temp_x + 2.0, hero.y0 + 120.0, 20.0, TextWeight::Semibold, c.text_primary);
        let feels = snap.temp_in(cur.apparent, units).round() as i64;
        text_left(
            scene,
            tr,
            &format!("Feels like {feels}{}", units.temp_suffix()),
            temp_x + 2.0,
            hero.y0 + 148.0,
            13.0,
            TextWeight::Regular,
            c.text_dim,
        );

        // Right side of hero: sunrise / sunset mini-panel.
        if let Some(today) = snap.daily.first() {
            let panel_x = hero.x1 - 230.0;
            let sr = hhmm(&today.sunrise);
            let ss = hhmm(&today.sunset);
            self.draw_sun_arc(scene, t, tr, theme, Rect::new(panel_x, hero.y0 + 24.0, hero.x1 - 24.0, hero.y0 + hero_h - 24.0), &sr, &ss, cur.is_day);
        }

        y += hero_h + 12.0;

        // ---- Metric tiles ----
        let tiles_h = 82.0;
        let gap = 12.0;
        let cols = 5.0;
        let tw = (mw - (cols - 1.0) * gap) / cols;
        let wind = snap.wind_in(cur.wind_speed, units).round() as i64;
        let tiles: [(&str, String, String); 5] = [
            ("WIND", format!("{wind}"), format!("{} {}", units::compass(cur.wind_direction), units.wind_suffix())),
            ("HUMIDITY", format!("{}", cur.humidity.round() as i64), "%".to_string()),
            ("PRECIP", format!("{:.1}", cur.precipitation), "mm now".to_string()),
            ("PRESSURE", format!("{}", cur.pressure.round() as i64), "hPa".to_string()),
            ("UV INDEX", format!("{}", cur.uv_index.round() as i64), uv_word(cur.uv_index).to_string()),
        ];
        for (i, (label, value, sub)) in tiles.iter().enumerate() {
            let tx = mx + i as f64 * (tw + gap);
            let tile = Rect::new(tx, y, tx + tw, y + tiles_h);
            card(scene, t, tile, theme);
            text_left(scene, tr, label, tile.x0 + 12.0, tile.y0 + 12.0, 10.0, TextWeight::Semibold, c.text_dim);
            text_left(scene, tr, value, tile.x0 + 12.0, tile.y0 + 32.0, 26.0, TextWeight::Bold, c.text_primary);
            text_left(scene, tr, sub, tile.x0 + 12.0, tile.y0 + tiles_h - 22.0, 11.0, TextWeight::Regular, c.text_dim);
        }

        y += tiles_h + 12.0;

        // ---- Hourly chart ----
        let hourly_h = 168.0;
        let hcard = Rect::new(mx, y, mx + mw, y + hourly_h);
        card(scene, t, hcard, theme);
        text_left(scene, tr, "Next 24 hours", hcard.x0 + 16.0, hcard.y0 + 12.0, 14.0, TextWeight::Semibold, c.text_primary);
        let points: Vec<ChartPoint> = snap
            .hourly
            .iter()
            .enumerate()
            .map(|(i, h)| ChartPoint {
                label: h.hour_label(),
                temp: snap.temp_in(h.temperature, units),
                precip: h.precip_probability,
                is_now: i == 0,
            })
            .collect();
        chart::draw_temp_curve(
            scene,
            t,
            Rect::new(hcard.x0 + 20.0, hcard.y0 + 36.0, hcard.x1 - 20.0, hcard.y1 - 10.0),
            &points,
            accent,
            theme,
            tr,
            "°",
        );

        y += hourly_h + 12.0;

        // ---- 7-day forecast ----
        let dcard = Rect::new(mx, y, mx + mw, main.y1 - PAD);
        card(scene, t, dcard, theme);
        text_left(scene, tr, "7-day forecast", dcard.x0 + 16.0, dcard.y0 + 12.0, 14.0, TextWeight::Semibold, c.text_primary);

        // Week temp range for the mini bars.
        let (mut wmin, mut wmax) = (f64::INFINITY, f64::NEG_INFINITY);
        for d in &snap.daily {
            wmin = wmin.min(d.temp_min);
            wmax = wmax.max(d.temp_max);
        }
        if (wmax - wmin).abs() < 1.0 {
            wmax += 1.0;
            wmin -= 1.0;
        }

        let rows_top = dcard.y0 + 40.0;
        let rows_area = (dcard.y1 - 12.0) - rows_top;
        let row_h = rows_area / snap.daily.len().max(1) as f64;
        for (i, d) in snap.daily.iter().enumerate() {
            let ry = rows_top + i as f64 * row_h;
            let cy = ry + row_h / 2.0;
            if i > 0 {
                scene.fill(
                    Fill::NonZero,
                    t,
                    c.border_standard.with_alpha_factor(0.5),
                    None,
                    &Rect::new(dcard.x0 + 16.0, ry, dcard.x1 - 16.0, ry + 1.0),
                );
            }
            // Day name.
            let day_label = match i {
                0 => "Today".to_string(),
                1 => "Tomorrow".to_string(),
                _ => weekday(&d.date),
            };
            text_left(scene, tr, &day_label, dcard.x0 + 18.0, cy - 8.0, 14.0, TextWeight::Medium, c.text_primary);
            // Icon.
            let dsky = Sky::from_code(d.weather_code);
            icon::draw_icon(
                scene,
                t,
                dsky,
                true,
                Rect::new(dcard.x0 + 120.0, cy - 14.0, dcard.x0 + 148.0, cy + 14.0),
                theme,
            );
            // Condition text.
            text_left(scene, tr, Sky::label(d.weather_code), dcard.x0 + 158.0, cy - 7.0, 13.0, TextWeight::Regular, c.text_dim);
            // Precip probability with a drop.
            let prec = format!("{}%", d.precip_probability.round() as i64);
            let pdx = dcard.x1 - 360.0;
            scene.fill(Fill::NonZero, t, c.semantic.info, None, &Circle::new(Point::new(pdx, cy), 3.0));
            text_left(scene, tr, &prec, pdx + 8.0, cy - 7.0, 12.0, TextWeight::Medium, c.semantic.info);

            // Range bar.
            let lo = snap.temp_in(d.temp_min, units).round() as i64;
            let hi = snap.temp_in(d.temp_max, units).round() as i64;
            let bar_x0 = dcard.x1 - 210.0;
            let bar_x1 = dcard.x1 - 70.0;
            text_right(scene, tr, &format!("{lo}°"), bar_x0 - 10.0, cy - 8.0, 14.0, TextWeight::Medium, c.text_dim);
            let track = RoundedRect::new(bar_x0, cy - 3.0, bar_x1, cy + 3.0, 3.0);
            scene.fill(Fill::NonZero, t, c.neutral[2], None, &track);
            let f0 = ((d.temp_min - wmin) / (wmax - wmin)).clamp(0.0, 1.0);
            let f1 = ((d.temp_max - wmin) / (wmax - wmin)).clamp(0.0, 1.0);
            let seg_x0 = bar_x0 + f0 * (bar_x1 - bar_x0);
            let seg_x1 = bar_x0 + f1 * (bar_x1 - bar_x0);
            let seg = RoundedRect::new(seg_x0, cy - 3.0, seg_x1.max(seg_x0 + 4.0), cy + 3.0, 3.0);
            let cold = c.semantic.info;
            let warm = c.semantic.warning;
            let grad = Gradient::new_linear(Point::new(bar_x0, cy), Point::new(bar_x1, cy))
                .with_stops([(0.0_f32, cold), (1.0_f32, warm)]);
            scene.fill(Fill::NonZero, t, &Brush::Gradient(grad), None, &seg);
            text_left(scene, tr, &format!("{hi}°"), bar_x1 + 10.0, cy - 8.0, 14.0, TextWeight::Semibold, c.text_primary);
        }
    }

    /// A small sunrise→sunset arc with the two times, for the hero's right
    /// flank. Reads warm/dim colors from the theme.
    #[allow(clippy::too_many_arguments)]
    fn draw_sun_arc(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        area: Rect,
        sunrise: &str,
        sunset: &str,
        is_day: bool,
    ) {
        let c = &theme.tokens.colors;
        let cx = (area.x0 + area.x1) / 2.0;
        let baseline = area.y1 - 24.0;
        let radius = (area.width() / 2.0 - 16.0).min(70.0);
        // Dashed horizon.
        scene.fill(
            Fill::NonZero,
            t,
            c.border_standard,
            None,
            &Rect::new(area.x0 + 8.0, baseline, area.x1 - 8.0, baseline + 1.0),
        );
        // Arc (semicircle) approximated by a polyline.
        let mut arc = BezPath::new();
        let steps = 28;
        for i in 0..=steps {
            let frac = i as f64 / steps as f64;
            let ang = std::f64::consts::PI * frac;
            let px = cx - radius * ang.cos();
            let py = baseline - radius * ang.sin();
            if i == 0 {
                arc.move_to(Point::new(px, py));
            } else {
                arc.line_to(Point::new(px, py));
            }
        }
        scene.stroke(&Stroke::new(1.5), t, c.text_dim.with_alpha_factor(0.7), None, &arc);
        // Sun position: midday if day, low if night.
        let frac = if is_day { 0.5 } else { 0.08 };
        let ang = std::f64::consts::PI * frac;
        let sx = cx - radius * ang.cos();
        let sy = baseline - radius * ang.sin();
        let sun_color = if is_day { c.semantic.warning } else { c.neutral[7] };
        scene.fill(Fill::NonZero, t, sun_color, None, &Circle::new(Point::new(sx, sy), 7.0));
        // Labels.
        text_left(scene, tr, "Sunrise", area.x0 + 4.0, baseline + 6.0, 10.0, TextWeight::Semibold, c.text_dim);
        text_left(scene, tr, sunrise, area.x0 + 4.0, baseline + 20.0, 13.0, TextWeight::Medium, c.text_primary);
        text_right(scene, tr, "Sunset", area.x1 - 4.0, baseline + 6.0, 10.0, TextWeight::Semibold, c.text_dim);
        text_right(scene, tr, sunset, area.x1 - 4.0, baseline + 20.0, 13.0, TextWeight::Medium, c.text_primary);
    }

    // ---- events ------------------------------------------------------

    /// Apply a hotspot action. Returns true if the model changed (persist).
    fn apply_action(&mut self, action: Action) -> bool {
        match action {
            Action::SelectLocation(i) => {
                self.model.select(i);
                self.refresh_selected();
                true
            }
            Action::RemoveLocation(i) => {
                self.model.remove_location(i);
                true
            }
            Action::SetTemp(u) => {
                if self.model.units.temp != u {
                    self.model.units.temp = u;
                    true
                } else {
                    false
                }
            }
            Action::SetWind(u) => {
                if self.model.units.wind != u {
                    self.model.units.wind = u;
                    true
                } else {
                    false
                }
            }
            Action::FocusSearch => {
                self.search_focused = true;
                false
            }
            Action::PickResult(i) => {
                if let Some(loc) = self.search_results.get(i).cloned() {
                    self.model.add_location(loc);
                    self.refresh_selected();
                    self.search_focused = false;
                    self.search_query.clear();
                    self.search_results.clear();
                    return true;
                }
                false
            }
        }
    }
}

impl Default for WeatherApp {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for WeatherApp {
    fn sync(&mut self, state: &mut AutoCommit, _path: &ObjId) {
        // First sync: seed the CRDT from the in-memory (sample) model if the
        // document is empty, otherwise adopt whatever is already persisted.
        if !self.seeded_doc {
            if WeatherDoc::is_initialised(state) {
                let persisted = WeatherDoc::read_model(state);
                if !persisted.locations.is_empty() {
                    // Keep any sample snapshots we already have for places the
                    // persisted model lacks; persisted snapshots win.
                    let mut merged = persisted;
                    for (id, snap) in self.model.snapshots.drain() {
                        merged.snapshots.entry(id).or_insert(snap);
                    }
                    self.model = merged;
                }
            } else {
                WeatherDoc::write_model(state, &self.model);
            }
            self.seeded_doc = true;
        }

        // Fold in any network results that landed since the last frame and
        // persist the freshest snapshot.
        if self.poll_service() && self.pending_doc_write {
            WeatherDoc::write_model(state, &self.model);
            self.pending_doc_write = false;
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // One full-bleed node; all internal layout is arithmetic in `draw`.
        let node = taffy
            .new_leaf(Style {
                size: Size { width: Dimension::percent(1.0), height: Dimension::percent(1.0) },
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        self.root_node = Some(node);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        if let Ok(layout) = taffy.layout(node) {
            self.rect = Rect::new(
                origin.x + layout.location.x as f64,
                origin.y + layout.location.y as f64,
                origin.x + layout.location.x as f64 + layout.size.width as f64,
                origin.y + layout.location.y as f64 + layout.size.height as f64,
            );
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
        // Fold any freshly-arrived network data into the model so it shows
        // this frame even if `sync` hasn't run (the live event loop keeps
        // redrawing while `needs_animation_frame` is true).
        self.poll_service();

        let mut hot: Vec<Hotspot> = Vec::new();
        self.draw_background(scene, transform, theme);
        self.draw_main(scene, transform, text_renderer, theme);
        self.draw_left_rail(scene, transform, text_renderer, theme, &mut hot);
        self.hotspots = hot;
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        _modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        _path: &ObjId,
        handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        use winit::event::{ElementState, KeyEvent, MouseButton};
        use winit::keyboard::{Key, NamedKey};

        // Spawn the network worker + initial fetch on the first input.
        self.ensure_service();

        let mut model_changed = false;

        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_mouse = Point::new(position.x / scale_factor, position.y / scale_factor);
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                // Topmost matching hotspot wins (rail results/remove are
                // pushed after the chips, so later entries take priority).
                let hit = self
                    .hotspots
                    .iter()
                    .rev()
                    .find(|h| h.rect.contains(self.last_mouse))
                    .map(|h| h.action.clone());
                if let Some(action) = hit {
                    model_changed = self.apply_action(action);
                    *handled = true;
                } else {
                    // Click outside the search field unfocuses it.
                    if self.search_focused {
                        self.search_focused = false;
                        *handled = true;
                    }
                }
            }
            WindowEvent::KeyboardInput {
                event: KeyEvent { logical_key, state: ElementState::Pressed, text, .. },
                ..
            } => {
                if self.search_focused {
                    match logical_key {
                        Key::Named(NamedKey::Escape) => {
                            self.search_focused = false;
                            self.search_query.clear();
                            self.search_results.clear();
                            *handled = true;
                        }
                        Key::Named(NamedKey::Backspace) => {
                            self.search_query.pop();
                            *handled = true;
                        }
                        Key::Named(NamedKey::Enter) => {
                            self.trigger_geocode();
                            *handled = true;
                        }
                        _ => {
                            if let Some(txt) = text {
                                let s = txt.as_str();
                                if !s.is_empty() && !s.chars().any(|ch| ch.is_control()) {
                                    self.search_query.push_str(s);
                                    *handled = true;
                                }
                            }
                        }
                    }
                } else {
                    match logical_key {
                        Key::Named(NamedKey::ArrowLeft) | Key::Named(NamedKey::ArrowUp) => {
                            self.model.select_relative(-1);
                            self.refresh_selected();
                            model_changed = true;
                            *handled = true;
                        }
                        Key::Named(NamedKey::ArrowRight) | Key::Named(NamedKey::ArrowDown) => {
                            self.model.select_relative(1);
                            self.refresh_selected();
                            model_changed = true;
                            *handled = true;
                        }
                        Key::Character(ch) => match ch.as_str() {
                            "u" | "U" => {
                                self.model.toggle_temp_unit();
                                model_changed = true;
                                *handled = true;
                            }
                            "w" | "W" => {
                                self.model.toggle_wind_unit();
                                model_changed = true;
                                *handled = true;
                            }
                            "r" | "R" => {
                                self.refresh_all();
                                *handled = true;
                            }
                            "/" => {
                                self.search_focused = true;
                                *handled = true;
                            }
                            _ => {}
                        },
                        _ => {}
                    }
                }
            }
            _ => {}
        }

        if model_changed {
            WeatherDoc::write_model(state, &self.model);
        }
    }

    fn name(&self) -> &str {
        "WeatherApp"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn needs_animation_frame(&self) -> bool {
        // Keep the event loop pumping frames while a fetch is in flight (so
        // results surface promptly), but stop after a deadline so a dead
        // network doesn't spin the CPU forever.
        self.inflight > 0 && self.last_request_at.elapsed().as_secs() < 12
    }

    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        _permissions: &PermissionContext,
    ) {
        // Root fills the window — never cull.
        self.draw(scene, transform, text_renderer, theme, presence);
    }
}

// =====================================================================
// Free helpers
// =====================================================================

fn card(scene: &mut Scene, t: Affine, rect: Rect, theme: &Theme) {
    let c = &theme.tokens.colors;
    let shape = RoundedRect::from_rect(rect, theme.tokens.radii.lg as f64);
    scene.fill(Fill::NonZero, t, c.surfaces.raised, None, &shape);
    scene.stroke(&Stroke::new(1.0), t, c.border_standard.with_alpha_factor(0.7), None, &shape);
}

#[allow(clippy::too_many_arguments)]
fn text_left(
    scene: &mut Scene,
    tr: &mut TextRenderer,
    s: &str,
    x: f64,
    y_top: f64,
    size: f32,
    weight: TextWeight,
    color: Color,
) {
    tr.draw_text_weighted(scene, s, size, color, Affine::translate((x, y_top)), None, weight);
}

#[allow(clippy::too_many_arguments)]
fn text_right(
    scene: &mut Scene,
    tr: &mut TextRenderer,
    s: &str,
    x_right: f64,
    y_top: f64,
    size: f32,
    weight: TextWeight,
    color: Color,
) {
    let (w, _) = tr.measure_weighted(s, size, None, weight);
    tr.draw_text_weighted(
        scene,
        s,
        size,
        color,
        Affine::translate((x_right - w as f64, y_top)),
        None,
        weight,
    );
}

#[allow(clippy::too_many_arguments)]
fn text_center(
    scene: &mut Scene,
    tr: &mut TextRenderer,
    s: &str,
    x_center: f64,
    y_top: f64,
    size: f32,
    weight: TextWeight,
    color: Color,
) {
    let (w, _) = tr.measure_weighted(s, size, None, weight);
    tr.draw_text_weighted(
        scene,
        s,
        size,
        color,
        Affine::translate((x_center - w as f64 / 2.0, y_top)),
        None,
        weight,
    );
}

fn lerp_color(a: Color, b: Color, t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    let a = a.to_rgba8();
    let b = b.to_rgba8();
    let mix = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t) as u8;
    Color::from_rgba8(mix(a.r, b.r), mix(a.g, b.g), mix(a.b, b.b), mix(a.a, b.a))
}

/// "HH:MM" from an ISO8601 "YYYY-MM-DDTHH:MM[...]" string.
fn hhmm(iso: &str) -> String {
    iso.get(11..16).unwrap_or("--:--").to_string()
}

/// Short weekday ("Mon") from a "YYYY-MM-DD" date.
fn weekday(date: &str) -> String {
    use chrono::NaiveDate;
    NaiveDate::parse_from_str(date, "%Y-%m-%d")
        .map(|d| d.format("%a").to_string())
        .unwrap_or_else(|_| date.to_string())
}

/// "Mon, Jun 30" header date from an ISO datetime.
fn fmt_date(iso: &str) -> String {
    use chrono::NaiveDate;
    let date_part = iso.get(0..10).unwrap_or(iso);
    NaiveDate::parse_from_str(date_part, "%Y-%m-%d")
        .map(|d| d.format("%a, %b %d").to_string())
        .unwrap_or_else(|_| date_part.to_string())
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
