//! Root widget for the weather app.
//!
//! Layout is done **manually** in `draw` from a single root rect (the whole
//! window). The two regions are:
//!
//! * a **flap sidebar** (app title + saved places + unit toggles) that is
//!   either docked beside the content or overlaid over it, depending on the
//!   window width (see [`crate::sidebar`]); and
//! * a flexible **content column** (header, current-conditions hero, metric
//!   tiles, the 24h temperature chart, and the 7-day list).
//!
//! Because everything is positioned arithmetically from `self.rect`, a
//! freshly fetched snapshot needs no taffy rebuild — the next `draw` simply
//! reads the new model.
//!
//! ## Direct manipulation of saved places
//! The saved-place chips support drag-to-reorder, swipe-left-to-delete, a
//! drag-onto-a-trash drop zone, and a right-click context menu. That state
//! lives in [`crate::interaction`]; this widget advances it from raw winit
//! events (it has no child-widget tree) and renders it each frame.
//!
//! ## The `--shots` / live split
//! `--shots` renders exactly one offscreen frame with no event loop and no
//! Tokio runtime, so it can't await a network fetch. The widget is therefore
//! seeded with bundled [`crate::sample`] data and renders that immediately.
//! The background [`crate::net::WeatherService`] is spawned **lazily on the
//! first real input event** — which `--shots` never delivers — so a
//! screenshot is fully populated and completely offline, while the live
//! binary fetches real Open-Meteo data and overwrites the sample snapshots
//! in place.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Circle, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Brush, Color, Fill, Gradient};
use winit::event::WindowEvent;

use ui::Widget;
use ui::color_compat::ColorCompat;
use ui::icons::Icon;
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;

use crate::chart::{self, ChartPoint};
use crate::doc::WeatherDoc;
use crate::icon;
use crate::interaction::{
    ContextMenu, EntryAction, PointerPress, SWIPE_COMMIT_DX,
};
use crate::model::WeatherModel;
use crate::net::{Request, Response, WeatherService};
use crate::sidebar::{SidebarMode, SidebarState, RAIL_W};
use crate::units::{self, TempUnit, WindUnit};
use crate::wxcode::Sky;

const PAD: f64 = 28.0;
const RP: f64 = 20.0; // rail inner padding
const CHIP_H: f64 = 62.0; // saved-place chip height
const CHIP_GAP: f64 = 8.0;
/// How long a recently-typed query stays "fresh" before we fire a geocode.
/// Short enough to feel instant, long enough to coalesce fast typing into a
/// single request.
const GEOCODE_DEBOUNCE: Duration = Duration::from_millis(220);

/// A click target rebuilt every `draw` and hit-tested in `handle_event`.
struct Hotspot {
    rect: Rect,
    action: Action,
}

#[derive(Clone)]
enum Action {
    SelectLocation(usize),
    SetTemp(TempUnit),
    SetWind(WindUnit),
    FocusSearch,
    PickResult(usize),
    ToggleSidebar,
    /// Tapping the scrim (overlay mode) dismisses the rail.
    DismissOverlay,
    /// A context-menu row resolving to a per-entry action.
    MenuItem(usize, EntryAction),
    /// Begin a potential press on a saved-place chip (may become a
    /// click, a reorder drag, or a swipe).
    PressChip(usize),
}

pub struct WeatherApp {
    model: WeatherModel,
    rect: Rect,
    root_node: Option<NodeId>,
    last_mouse: Point,
    hotspots: Vec<Hotspot>,

    // Sidebar (Adwaita flap).
    sidebar: SidebarState,

    // Saved-place direct manipulation.
    press: Option<PointerPress>,
    menu: Option<ContextMenu>,
    /// Index a reorder drag would currently drop the entry at (for the
    /// insertion indicator). `None` when not reordering.
    drop_index: Option<usize>,

    // Search state (ephemeral UI — not collaborative).
    search_query: String,
    search_focused: bool,
    search_results: Vec<crate::model::SavedLocation>,
    search_seq: u64,
    /// Set on each keystroke; once it's older than [`GEOCODE_DEBOUNCE`] the
    /// next frame fires the geocode. `None` once a request has been sent.
    pending_query_at: Option<Instant>,
    /// Recent geocode results keyed by the trimmed lowercase query. Lets a
    /// re-typed or backspaced query show instantly without a round trip.
    geocode_cache: HashMap<String, Vec<crate::model::SavedLocation>>,

    // Networking (live only; None until first input event).
    service: Option<WeatherService>,
    initial_fetch_done: bool,
    inflight: usize,
    geocode_inflight: bool,
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
            sidebar: SidebarState::default(),
            press: None,
            menu: None,
            drop_index: None,
            search_query: String::new(),
            search_focused: false,
            search_results: Vec::new(),
            search_seq: 0,
            pending_query_at: None,
            geocode_cache: HashMap::new(),
            service: None,
            initial_fetch_done: false,
            inflight: 0,
            geocode_inflight: false,
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

    fn refresh_location(&mut self, idx: usize) {
        let Some(svc) = self.service.as_ref() else { return };
        let Some(loc) = self.model.locations.get(idx) else { return };
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

    // ---- geocoding (debounced + cached + background) -----------------

    /// Record a keystroke and either show a cached result instantly or arm
    /// the debounce timer. Never blocks; the network call is deferred to a
    /// later frame so typing stays responsive.
    fn on_query_changed(&mut self) {
        let key = self.search_query.trim().to_lowercase();
        if key.is_empty() {
            self.search_results.clear();
            self.pending_query_at = None;
            self.status = "Sample data — live updates on launch".to_string();
            return;
        }
        // Instant feedback from the cache, if we've seen this query.
        if let Some(hit) = self.geocode_cache.get(&key) {
            self.search_results = hit.clone();
            self.status = format!("{} matches", hit.len());
        } else {
            self.status = "Typing…".to_string();
        }
        // (Re)arm the debounce; the actual request fires once typing pauses.
        self.pending_query_at = Some(Instant::now());
    }

    /// If the debounce window has elapsed since the last keystroke, fire the
    /// geocode request now. Called once per frame. Returns true if a request
    /// was dispatched (so the caller keeps animating for the response).
    fn maybe_fire_geocode(&mut self) -> bool {
        let Some(at) = self.pending_query_at else { return false };
        if at.elapsed() < GEOCODE_DEBOUNCE {
            return false;
        }
        self.pending_query_at = None;
        let q = self.search_query.trim().to_string();
        if q.is_empty() {
            return false;
        }
        // A cache hit needs no network call (it's already shown).
        let key = q.to_lowercase();
        if self.geocode_cache.contains_key(&key) {
            return false;
        }
        self.ensure_service();
        if let Some(svc) = self.service.as_ref() {
            self.search_seq += 1;
            self.geocode_inflight = true;
            svc.request(Request::Geocode { seq: self.search_seq, query: q });
            self.status = "Searching…".to_string();
            return true;
        }
        false
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
                        self.geocode_inflight = false;
                        // Cache for instant re-display next time.
                        let key = self.search_query.trim().to_lowercase();
                        if !key.is_empty() {
                            self.geocode_cache.insert(key, results.clone());
                        }
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
                    self.geocode_inflight = false;
                    self.status = format!("Offline · {e}");
                    changed = true;
                }
            }
        }
        changed
    }

    // ---- per-entry actions (swipe ⇔ menu parity) ---------------------

    /// Resolve a per-entry [`EntryAction`] against the model. The single
    /// place both the swipe gesture and the context menu funnel into, so the
    /// two stay in lockstep. Returns true if the model changed.
    fn apply_entry_action(&mut self, index: usize, action: EntryAction) -> bool {
        match action {
            EntryAction::Delete => {
                if index < self.model.locations.len() {
                    self.model.remove_location(index);
                    true
                } else {
                    false
                }
            }
            EntryAction::MoveToTop => self.model.move_location(index, 0),
            EntryAction::Refresh => {
                self.refresh_location(index);
                false // a fetch in flight isn't a model change yet
            }
        }
    }

    // ---- geometry ----------------------------------------------------

    /// Resolve the current sidebar mode from the *allocated* width.
    fn sidebar_mode(&self) -> SidebarMode {
        SidebarMode::from_width(self.rect.width())
    }

    /// The content column's rect, given the current sidebar mode + reveal.
    /// Docked: content is inset by the revealed rail width. Overlay: content
    /// always spans the full width (the rail floats over it).
    fn content_rect(&self) -> Rect {
        match self.sidebar_mode() {
            SidebarMode::Docked => {
                let inset = RAIL_W * self.sidebar.reveal();
                Rect::new(self.rect.x0 + inset, self.rect.y0, self.rect.x1, self.rect.y1)
            }
            SidebarMode::Overlay => self.rect,
        }
    }

    /// The rail's current on-screen rect. Slides in from the left edge by the
    /// eased reveal amount; fully off-screen at reveal 0.
    fn rail_rect(&self) -> Rect {
        let slide = self.sidebar.slide();
        let x0 = self.rect.x0 - RAIL_W * (1.0 - slide);
        Rect::new(x0, self.rect.y0, x0 + RAIL_W, self.rect.y1)
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

    /// The hamburger / close toggle in the content's top-left corner. Shown
    /// whenever the rail isn't fully docked-open, so a collapsed rail can be
    /// summoned and an overlay rail dismissed. Returns its rect for layout.
    fn draw_sidebar_toggle(
        &self,
        scene: &mut Scene,
        t: Affine,
        theme: &Theme,
        content: Rect,
        hot: &mut Vec<Hotspot>,
    ) {
        let c = &theme.tokens.colors;
        let size = 34.0;
        let bx = content.x0 + PAD;
        let by = content.y0 + PAD;
        let btn = Rect::new(bx, by, bx + size, by + size);
        let shape = RoundedRect::from_rect(btn, theme.tokens.radii.md as f64);
        scene.fill(Fill::NonZero, t, c.surfaces.raised.with_alpha_factor(0.9), None, &shape);
        scene.stroke(&Stroke::new(1.0), t, c.border_standard, None, &shape);

        let cx = btn.x0 + size / 2.0;
        let line_w = 14.0;
        let open = self.sidebar.reveal() > 0.5;
        if open {
            // An "X"/close affordance when the rail is showing.
            let r = 6.0;
            let cyc = btn.y0 + size / 2.0;
            scene.stroke(&Stroke::new(1.8), t, c.text_primary, None,
                &Line::new(Point::new(cx - r, cyc - r), Point::new(cx + r, cyc + r)));
            scene.stroke(&Stroke::new(1.8), t, c.text_primary, None,
                &Line::new(Point::new(cx + r, cyc - r), Point::new(cx - r, cyc + r)));
        } else {
            // Three-line hamburger.
            for i in 0..3 {
                let ly = btn.y0 + 11.0 + i as f64 * 6.0;
                scene.stroke(&Stroke::new(1.8), t, c.text_primary, None,
                    &Line::new(Point::new(cx - line_w / 2.0, ly), Point::new(cx + line_w / 2.0, ly)));
            }
        }
        hot.push(Hotspot { rect: btn, action: Action::ToggleSidebar });
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
        let mode = self.sidebar_mode();
        let reveal = self.sidebar.reveal();
        if reveal <= 0.001 && mode == SidebarMode::Overlay {
            return; // fully collapsed overlay — nothing to paint
        }

        // Overlay mode dims the content behind a scrim that fades with the
        // reveal (Adwaita's flap dimming), and the rail floats above it.
        if mode == SidebarMode::Overlay {
            let scrim = c.surfaces.overlay.with_alpha_factor((reveal * 0.6) as f32);
            scene.fill(Fill::NonZero, t, scrim, None, &self.rect);
            hot.push(Hotspot { rect: self.rect, action: Action::DismissOverlay });
        }

        let rail = self.rail_rect();
        // Rail panel — a translucent raised surface separating it from the
        // gradient content area. Overlay mode is opaque (it floats over
        // content); docked mode is lightly translucent.
        let panel_alpha = if mode == SidebarMode::Overlay { 0.98 } else { 0.92 };
        scene.fill(Fill::NonZero, t, c.surfaces.raised.with_alpha_factor(panel_alpha), None, &rail);
        scene.stroke(
            &Stroke::new(1.0),
            t,
            c.border_standard,
            None,
            &Line::new(Point::new(rail.x1, rail.y0), Point::new(rail.x1, rail.y1)),
        );

        let x = rail.x0 + RP;
        let w = RAIL_W - 2.0 * RP;

        // Title — kept at the very top of the rail (decoration-less OS, and
        // mobile-compatible). A small close affordance sits at the right edge
        // so an overlay rail can be dismissed without reaching the scrim.
        text_left(scene, tr, "Weather", x, rail.y0 + 24.0, 22.0, TextWeight::Bold, c.text_primary);
        text_left(scene, tr, "Open-Meteo", x, rail.y0 + 52.0, 12.0, TextWeight::Regular, c.text_dim);
        {
            let cs = 26.0;
            let close = Rect::new(rail.x1 - RP - cs, rail.y0 + 18.0, rail.x1 - RP, rail.y0 + 18.0 + cs);
            if close.contains(self.last_mouse) {
                scene.fill(Fill::NonZero, t, c.neutral[2], None, &RoundedRect::from_rect(close, 6.0));
            }
            Icon::X.draw(scene, t, close.inset(-6.0), c.text_dim);
            hot.push(Hotspot { rect: close, action: Action::ToggleSidebar });
        }

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

        // Search results (live or cached). A tiny spinner-dot communicates an
        // in-flight request while still showing any cached rows underneath.
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
        } else if self.search_focused && self.geocode_inflight {
            text_left(scene, tr, "Searching…", x + 4.0, y, 12.0, TextWeight::Regular, c.text_dim);
            y += 24.0;
        }

        // Saved-places list — its bottom is bounded so the drag/unit panel at
        // the rail foot never overlaps it.
        text_left(scene, tr, "SAVED PLACES", x, y, 10.0, TextWeight::Semibold, c.text_dim);
        y += 22.0;
        self.draw_saved_places(scene, t, tr, theme, rail, x, w, y, hot);

        // Foot of the rail: either the unit toggles (normal) or the big
        // Delete drop zone (while a reorder drag is in progress).
        if self.is_reorder_active() {
            self.draw_delete_dropzone(scene, t, tr, theme, rail, hot);
        } else {
            self.draw_units(scene, t, tr, theme, rail, hot);
        }

        // The context menu paints last so it sits above the chips. (Kept in
        // `draw` rather than `draw_overlay` so the `--shots` path, which only
        // runs the normal draw pass, still captures it.)
        if self.menu.is_some() {
            self.draw_context_menu(scene, t, tr, theme, hot);
        }
    }

    /// Draw the saved-place chips, honouring an in-progress drag (the dragged
    /// chip follows the cursor; a gap opens at the drop index) or swipe (the
    /// swiped chip slides left over a red delete backing).
    #[allow(clippy::too_many_arguments)]
    fn draw_saved_places(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        rail: Rect,
        x: f64,
        w: f64,
        list_top: f64,
        hot: &mut Vec<Hotspot>,
    ) {
        let c = &theme.tokens.colors;
        let units = self.model.units;
        let n = self.model.locations.len();
        let dragging = self.dragged_index();
        let drop_at = self.drop_index;

        // Base slot y for entry i (without drag displacement).
        let slot_y = |i: usize| list_top + i as f64 * (CHIP_H + CHIP_GAP);

        // Insertion indicator while reordering.
        if let (Some(_), Some(di)) = (dragging, drop_at) {
            let gy = slot_y(di) - CHIP_GAP / 2.0;
            scene.fill(
                Fill::NonZero,
                t,
                self.accent(theme),
                None,
                &RoundedRect::new(x, gy - 1.5, x + w, gy + 1.5, 1.5),
            );
        }

        for (i, loc) in self.model.locations.iter().enumerate() {
            // The actively-dragged chip is drawn last (on top), skip here.
            if Some(i) == dragging {
                continue;
            }
            let y = slot_y(i);
            let chip = Rect::new(x, y, x + w, y + CHIP_H);

            // Swipe-left: reveal a red delete backing, slide the chip.
            let swipe_dx = self.swipe_dx_for(i);
            if swipe_dx > 0.0 {
                self.draw_swipe_backing(scene, t, theme, chip);
            }
            let chip_t = t * Affine::translate((-swipe_dx, 0.0));

            self.draw_chip(scene, chip_t, tr, theme, chip, loc, i, units);
            // Hotspot is the un-slid chip position (press starts the gesture).
            hot.push(Hotspot { rect: chip, action: Action::PressChip(i) });
        }

        // Trailing gap indicator (dropping at the end).
        if let (Some(_), Some(di)) = (dragging, drop_at) {
            if di == n {
                let gy = slot_y(n) - CHIP_GAP / 2.0;
                scene.fill(
                    Fill::NonZero,
                    t,
                    self.accent(theme),
                    None,
                    &RoundedRect::new(x, gy - 1.5, x + w, gy + 1.5, 1.5),
                );
            }
        }

        // Finally the floating dragged chip under the cursor.
        if let Some(i) = dragging {
            if let Some(loc) = self.model.locations.get(i) {
                let dy = self.last_mouse.y - CHIP_H / 2.0;
                let chip = Rect::new(x, dy, x + w, dy + CHIP_H);
                // Soft shadow so it reads as lifted.
                let sh = RoundedRect::from_rect(chip.inset(-2.0), theme.tokens.radii.md as f64 + 2.0);
                scene.fill(Fill::NonZero, t, Color::from_rgba8(0, 0, 0, 60), None, &sh);
                self.draw_chip(scene, t, tr, theme, chip, loc, i, units);
            }
        }

        let _ = rail;
        let _ = c;
    }

    /// Paint one saved-place chip (icon + name + temperature). `index` drives
    /// the selected highlight.
    #[allow(clippy::too_many_arguments)]
    fn draw_chip(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        chip: Rect,
        loc: &crate::model::SavedLocation,
        index: usize,
        units: crate::units::Units,
    ) {
        let c = &theme.tokens.colors;
        let selected = index == self.model.selected;
        let hovered = chip.contains(self.last_mouse);
        let bg = if selected {
            lerp_color(c.surfaces.base, self.accent(theme), 0.18)
        } else if hovered {
            c.neutral[2]
        } else {
            c.surfaces.base
        };
        let shape = RoundedRect::from_rect(chip, theme.tokens.radii.md as f64);
        scene.fill(Fill::NonZero, t, bg, None, &shape);
        if selected {
            scene.stroke(&Stroke::new(1.5), t, self.accent(theme), None, &shape);
        }
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
        // A subtle grip affordance on hover hints the chip is draggable.
        if hovered && !selected {
            let gx = chip.x1 - 16.0;
            for r in 0..2 {
                for d in 0..3 {
                    let p = Point::new(gx + r as f64 * 4.0, chip.y1 - 18.0 + d as f64 * 4.0);
                    scene.fill(Fill::NonZero, t, c.text_dim.with_alpha_factor(0.5), None, &Circle::new(p, 1.0));
                }
            }
        }
    }

    /// The red backing revealed under a chip as it's swiped left — a trash
    /// glyph on the danger colour, foreshadowing the delete.
    fn draw_swipe_backing(&self, scene: &mut Scene, t: Affine, theme: &Theme, chip: Rect) {
        let c = &theme.tokens.colors;
        let shape = RoundedRect::from_rect(chip, theme.tokens.radii.md as f64);
        scene.fill(Fill::NonZero, t, c.semantic.danger, None, &shape);
        let icon_r = Rect::new(chip.x1 - 42.0, chip.y0 + 17.0, chip.x1 - 14.0, chip.y0 + 45.0);
        Icon::Trash.draw(scene, t, icon_r, Color::WHITE);
    }

    /// The big "Delete" drop surface shown at the rail foot while a reorder
    /// drag is active — a red, rounded, dashed-outline target with a trash
    /// icon. Dropping a chip here deletes it (handled on release).
    fn draw_delete_dropzone(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        rail: Rect,
        _hot: &mut [Hotspot],
    ) {
        let c = &theme.tokens.colors;
        let x = rail.x0 + RP;
        let w = RAIL_W - 2.0 * RP;
        let h = 96.0;
        let zone = Rect::new(x, rail.y1 - h - RP, x + w, rail.y1 - RP);
        let armed = self.delete_zone_rect_armed(zone);

        let shape = RoundedRect::from_rect(zone, theme.tokens.radii.lg as f64);
        // Fill brightens when the cursor is over it (armed to delete).
        let fill = if armed {
            c.semantic.danger
        } else {
            c.semantic.danger.with_alpha_factor(0.16)
        };
        scene.fill(Fill::NonZero, t, fill, None, &shape);
        // Dashed / dotted ("punctuated") danger outline so it's unmistakably
        // a different kind of target than the solid unit controls.
        let dash = Stroke::new(2.0).with_dashes(0.0, [6.0, 5.0]);
        scene.stroke(&dash, t, c.semantic.danger, None, &shape);

        let icon_color = if armed { Color::WHITE } else { c.semantic.danger };
        let icon_r = Rect::new(
            zone.x0 + w / 2.0 - 16.0,
            zone.y0 + 14.0,
            zone.x0 + w / 2.0 + 16.0,
            zone.y0 + 46.0,
        );
        Icon::Trash.draw_with_stroke(scene, t, icon_r, icon_color, 2.0);
        let label_color = if armed { Color::WHITE } else { c.semantic.danger };
        text_center(scene, tr, "Delete", zone.x0 + w / 2.0, zone.y1 - 26.0, 14.0, TextWeight::Bold, label_color);
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

    /// Draw the open per-entry context menu at its anchor and register a
    /// hotspot per row.
    fn draw_context_menu(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        hot: &mut Vec<Hotspot>,
    ) {
        let Some(menu) = self.menu.as_ref() else { return };
        let c = &theme.tokens.colors;
        let mw = 184.0;
        let row_h = 34.0;
        let pad = 6.0;
        let mh = pad * 2.0 + menu.items.len() as f64 * row_h;
        // Keep the menu on-screen.
        let mx = menu.anchor.x.min(self.rect.x1 - mw - 4.0).max(self.rect.x0 + 4.0);
        let my = menu.anchor.y.min(self.rect.y1 - mh - 4.0).max(self.rect.y0 + 4.0);
        let panel = Rect::new(mx, my, mx + mw, my + mh);
        let shape = RoundedRect::from_rect(panel, theme.tokens.radii.md as f64);
        // Drop shadow + raised panel.
        scene.fill(Fill::NonZero, t, Color::from_rgba8(0, 0, 0, 80), None,
            &RoundedRect::from_rect(panel.inset(-3.0), theme.tokens.radii.md as f64 + 3.0));
        scene.fill(Fill::NonZero, t, c.surfaces.overlay.with_alpha_factor(0.98), None, &shape);
        scene.stroke(&Stroke::new(1.0), t, c.border_standard, None, &shape);

        for (i, action) in menu.items.iter().enumerate() {
            let ry = my + pad + i as f64 * row_h;
            let row = Rect::new(mx + pad, ry, mx + mw - pad, ry + row_h);
            let hovered = row.contains(self.last_mouse);
            if hovered {
                let hl = if action.is_destructive() {
                    c.semantic.danger.with_alpha_factor(0.22)
                } else {
                    c.neutral[2]
                };
                scene.fill(Fill::NonZero, t, hl, None, &RoundedRect::from_rect(row, 6.0));
            }
            let text_color = if action.is_destructive() { c.semantic.danger } else { c.text_primary };
            // Leading icon.
            let icon_r = Rect::new(row.x0 + 6.0, row.y0 + 8.0, row.x0 + 24.0, row.y0 + 26.0);
            match action {
                EntryAction::Delete => Icon::Trash.draw(scene, t, icon_r, text_color),
                EntryAction::MoveToTop => Icon::ChevronUp.draw(scene, t, icon_r, text_color),
                EntryAction::Refresh => Icon::Refresh.draw(scene, t, icon_r, text_color),
            }
            text_left(scene, tr, action.label(), row.x0 + 32.0, row.y0 + row_h / 2.0 - 8.0, 13.0, TextWeight::Medium, text_color);
            hot.push(Hotspot { rect: row, action: Action::MenuItem(menu.index, *action) });
        }
    }

    fn draw_main(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        content: Rect,
    ) {
        let c = &theme.tokens.colors;
        let main = content;
        // When the toggle button is shown (rail not fully docked-open) the
        // header is nudged right so the title clears the button.
        let toggle_inset = if self.show_toggle() { 46.0 } else { 0.0 };
        let mx = main.x0 + PAD + toggle_inset;
        let mw = main.width() - 2.0 * PAD - toggle_inset;
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

    // ---- drag / swipe helpers ----------------------------------------

    /// True while a vertical reorder drag is in progress (the foot of the
    /// rail shows the Delete drop zone in this state).
    fn is_reorder_active(&self) -> bool {
        self.press.map(|p| p.is_reorder()).unwrap_or(false)
    }

    /// Index of the chip currently being lifted by a reorder drag.
    fn dragged_index(&self) -> Option<usize> {
        self.press.filter(|p| p.is_reorder()).map(|p| p.index)
    }

    /// Leftward swipe distance to apply to chip `i` (0 unless it's the chip
    /// under an active horizontal swipe).
    fn swipe_dx_for(&self, i: usize) -> f64 {
        match self.press {
            Some(p) if p.is_swipe() && p.index == i => p.swipe_dx(),
            _ => 0.0,
        }
    }

    /// Whether the cursor is over the delete drop zone (so it renders armed).
    fn delete_zone_rect_armed(&self, zone: Rect) -> bool {
        self.is_reorder_active() && zone.contains(self.last_mouse)
    }

    /// The delete drop zone's rect at the rail foot (recomputed for hit-test).
    fn delete_zone_rect(&self) -> Rect {
        let rail = self.rail_rect();
        let x = rail.x0 + RP;
        let w = RAIL_W - 2.0 * RP;
        let h = 96.0;
        Rect::new(x, rail.y1 - h - RP, x + w, rail.y1 - RP)
    }

    /// Compute the drop index for a reorder drag from the cursor's y. Returns
    /// an insertion index in `0..=len`.
    fn reorder_drop_index(&self, list_top: f64) -> usize {
        let n = self.model.locations.len();
        let rel = (self.last_mouse.y - list_top) / (CHIP_H + CHIP_GAP);
        ((rel.round()).max(0.0) as usize).min(n)
    }

    /// The y at which the saved-places list begins (mirrors `draw_left_rail`).
    fn saved_places_top(&self) -> f64 {
        let rail = self.rail_rect();
        let mut y = rail.y0 + 78.0 + 38.0 + 12.0; // title block + search field
        if self.search_focused && !self.search_results.is_empty() {
            y += self.search_results.len().min(6) as f64 * 36.0 + 6.0;
        } else if self.search_focused && self.geocode_inflight {
            y += 24.0;
        }
        y += 22.0; // "SAVED PLACES" header
        y
    }

    /// Whether the hamburger toggle is currently shown (rail not fully
    /// docked-open).
    fn show_toggle(&self) -> bool {
        self.sidebar_mode() == SidebarMode::Overlay || self.sidebar.reveal() < 0.999
    }

    // ---- events ------------------------------------------------------

    /// Apply a hotspot action. Returns true if the model changed (persist).
    fn apply_action(&mut self, action: Action) -> bool {
        match action {
            Action::SelectLocation(i) => {
                self.model.select(i);
                self.refresh_selected();
                // In overlay mode, picking a place closes the flap (Adwaita).
                if self.sidebar_mode() == SidebarMode::Overlay {
                    self.sidebar.close();
                }
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
                    self.pending_query_at = None;
                    return true;
                }
                false
            }
            Action::ToggleSidebar => {
                self.sidebar.toggle(self.sidebar_mode());
                false
            }
            Action::DismissOverlay => {
                self.sidebar.close();
                false
            }
            Action::MenuItem(index, action) => {
                self.menu = None;
                self.apply_entry_action(index, action)
            }
            Action::PressChip(_) => false, // press handled in mouse-down path
        }
    }
}

impl WeatherApp {
    /// Seed a deterministic interaction state for a headless screenshot.
    /// `--shots` renders a single frame with no event loop, so a scenario
    /// that would normally require live input (an in-progress drag, an open
    /// flap) is staged here instead. Unknown names render the default frame.
    ///
    /// Recognised scenarios:
    /// * `"drag"` — a reorder drag of the second saved place is in flight,
    ///   the cursor parked low so the rail foot shows the red dashed Delete
    ///   drop surface.
    /// * `"overlay"` — a narrow-window flap opened over the content with its
    ///   dimming scrim (best paired with a narrow viewport).
    pub fn seed_shot_scenario(&mut self, scenario: &str) {
        match scenario {
            "drag" => {
                // Lift the 2nd chip and classify the press as a vertical
                // reorder. Park the cursor mid-list (NOT over the drop zone)
                // so the lifted chip floats clearly and the Delete surface
                // shows its un-armed dashed outline.
                let mut press = PointerPress::new(1, Point::new(150.0, 250.0));
                press.update(Point::new(150.0, 470.0)); // mostly-vertical → reorder
                self.last_mouse = Point::new(150.0, 470.0);
                self.drop_index = Some(self.model.locations.len());
                self.press = Some(press);
            }
            "overlay" => {
                // Force the flap open; mode resolves to overlay at narrow
                // widths during layout, so this shows the scrim + float.
                self.sidebar.toggle(SidebarMode::Overlay);
                self.last_mouse = Point::new(150.0, 250.0);
            }
            _ => {}
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
        // Mode is now known from the allocated width — reconcile the flap so a
        // breakpoint crossing adopts the new default (wide ⇒ open, narrow ⇒
        // closed) before the next draw.
        self.sidebar.reconcile(self.sidebar_mode());
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
        // Advance the flap transition and fire any due geocode.
        self.sidebar.reconcile(self.sidebar_mode());
        self.sidebar.tick(self.sidebar_mode());
        self.maybe_fire_geocode();

        let content = self.content_rect();
        let mut hot: Vec<Hotspot> = Vec::new();
        self.draw_background(scene, transform, theme);
        self.draw_main(scene, transform, text_renderer, theme, content);
        // The hamburger/close toggle lives over the content's corner.
        if self.show_toggle() {
            self.draw_sidebar_toggle(scene, transform, theme, content, &mut hot);
        }
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
                // Advance an in-progress press (classify click/reorder/swipe).
                if let Some(press) = self.press.as_mut() {
                    press.update(self.last_mouse);
                    if press.is_reorder() {
                        let top = self.saved_places_top();
                        self.drop_index = Some(self.reorder_drop_index(top));
                    }
                }
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Right, .. } => {
                // Right-click on a saved-place chip opens the context menu
                // (winit's MouseInput carries no coords — we use the cached
                // cursor position).
                if let Some(idx) = self.chip_at(self.last_mouse) {
                    self.menu = Some(ContextMenu::for_entry(idx, self.last_mouse));
                    *handled = true;
                } else if self.menu.is_some() {
                    self.menu = None;
                    *handled = true;
                }
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                // A click while a menu is open: a menu row consumes it,
                // anything else dismisses the menu.
                if self.menu.is_some() {
                    let hit = self
                        .hotspots
                        .iter()
                        .rev()
                        .find(|h| matches!(h.action, Action::MenuItem(..)) && h.rect.contains(self.last_mouse))
                        .map(|h| h.action.clone());
                    if let Some(action) = hit {
                        model_changed = self.apply_action(action);
                    } else {
                        self.menu = None;
                    }
                    *handled = true;
                } else {
                    // Topmost matching hotspot wins.
                    let hit = self
                        .hotspots
                        .iter()
                        .rev()
                        .find(|h| h.rect.contains(self.last_mouse))
                        .map(|h| h.action.clone());
                    match hit {
                        Some(Action::PressChip(i)) => {
                            // Arm a press; classification happens on move.
                            self.press = Some(PointerPress::new(i, self.last_mouse));
                            self.drop_index = None;
                            *handled = true;
                        }
                        Some(action) => {
                            model_changed = self.apply_action(action);
                            *handled = true;
                        }
                        None => {
                            if self.search_focused {
                                self.search_focused = false;
                                *handled = true;
                            }
                        }
                    }
                }
            }
            WindowEvent::MouseInput { state: ElementState::Released, button: MouseButton::Left, .. } => {
                if let Some(press) = self.press.take() {
                    self.drop_index = None;
                    *handled = true;
                    if press.is_reorder() {
                        // Drop on the delete zone → delete; else reorder.
                        if self.delete_zone_rect().contains(self.last_mouse) {
                            model_changed = self.apply_entry_action(press.index, EntryAction::Delete);
                        } else {
                            let top = self.saved_places_top();
                            let mut to = self.reorder_drop_index(top);
                            // Inserting after the original slot shifts by one.
                            if to > press.index {
                                to -= 1;
                            }
                            model_changed = self.model.move_location(press.index, to);
                        }
                    } else if press.is_swipe() {
                        if press.swipe_dx() >= SWIPE_COMMIT_DX {
                            model_changed = self.apply_entry_action(press.index, EntryAction::swipe_left());
                        }
                    } else {
                        // Treated as a plain click → select.
                        model_changed = self.apply_action(Action::SelectLocation(press.index));
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
                            self.pending_query_at = None;
                            *handled = true;
                        }
                        Key::Named(NamedKey::Backspace) => {
                            self.search_query.pop();
                            self.on_query_changed();
                            *handled = true;
                        }
                        Key::Named(NamedKey::Enter) => {
                            // Enter skips the debounce — fire immediately.
                            self.pending_query_at = Some(Instant::now() - GEOCODE_DEBOUNCE);
                            *handled = true;
                        }
                        _ => {
                            if let Some(txt) = text {
                                let s = txt.as_str();
                                if !s.is_empty() && !s.chars().any(|ch| ch.is_control()) {
                                    self.search_query.push_str(s);
                                    self.on_query_changed();
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
                        Key::Named(NamedKey::Escape) => {
                            // Esc closes a menu or an overlay flap.
                            if self.menu.is_some() {
                                self.menu = None;
                                *handled = true;
                            } else if self.sidebar_mode() == SidebarMode::Overlay {
                                self.sidebar.close();
                                *handled = true;
                            }
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
                            "b" | "B" => {
                                // Toggle the sidebar flap.
                                self.sidebar.toggle(self.sidebar_mode());
                                *handled = true;
                            }
                            "/" => {
                                self.search_focused = true;
                                // Opening search in overlay mode reveals the rail.
                                if self.sidebar_mode() == SidebarMode::Overlay {
                                    self.sidebar.toggle(self.sidebar_mode());
                                }
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
        // Keep the event loop pumping frames while:
        //  * a fetch is in flight (so results surface promptly), bounded so a
        //    dead network doesn't spin the CPU forever;
        //  * the sidebar flap is mid-transition;
        //  * a geocode debounce is pending (so it fires without an extra event).
        let fetching = self.inflight > 0 && self.last_request_at.elapsed().as_secs() < 12;
        let animating = (self.sidebar.reveal() - if self.sidebar.is_open(self.sidebar_mode()) { 1.0 } else { 0.0 }).abs() > 0.001;
        let debouncing = self.pending_query_at.is_some();
        fetching || animating || debouncing
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

impl WeatherApp {
    /// The saved-place chip index under `p`, if any. Used for right-click and
    /// to distinguish chip presses. Mirrors the chip layout in `draw`.
    fn chip_at(&self, p: Point) -> Option<usize> {
        let rail = self.rail_rect();
        // Only chips inside a revealed rail are hittable.
        if self.sidebar.reveal() <= 0.01 {
            return None;
        }
        let x = rail.x0 + RP;
        let w = RAIL_W - 2.0 * RP;
        let top = self.saved_places_top();
        for i in 0..self.model.locations.len() {
            let y = top + i as f64 * (CHIP_H + CHIP_GAP);
            if Rect::new(x, y, x + w, y + CHIP_H).contains(p) {
                return Some(i);
            }
        }
        None
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
