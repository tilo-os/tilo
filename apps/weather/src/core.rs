//! Shared weather state and the two **content panels** it renders.
//!
//! Per the HIG §1b, weather is *not* a sidebar+content app — it is an
//! **overview + detail** pair of content panels:
//!
//! * the **overview** is a *contentful* city list (each row carries quick
//!   per-city info — condition icon + current temperature); and
//! * the **detail** is the full forecast for the selected city.
//!
//! The responsive shell ([`ui::widget::NavShell`]) owns the breakpoint,
//! the side-by-side↔push transition, the Back affordance, and the divider.
//! This module owns the *content* of the two panes plus the state they
//! share (the model, the search box, direct-manipulation of the list) and
//! the network request queue the app pumps.
//!
//! [`Core`] is held once behind an `Rc<RefCell<…>>`; the two pane widgets
//! (see [`crate::panes`]) borrow it to draw and to route input, and the root
//! [`crate::app::WeatherApp`] borrows it to seed the CRDT, pump the network
//! worker, and drive the shell.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use automerge::{AutoCommit, ObjId};
use taffy::{NodeId, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Circle, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{BlendMode, Brush, Color, Fill, Gradient};
use winit::event::WindowEvent;

use ui::color_compat::ColorCompat;
use ui::icons::Icon;
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::{
    Autocomplete, AutocompleteOutcome, DragController, ListMetrics, PressOutcome, ScrollMetrics,
    Scrollbar, Suggestion, Widget,
};

use crate::api;
use crate::chart;
use crate::icon;
use crate::interaction::{ContextMenu, EntryAction, PointerPress, SWIPE_COMMIT_DX};
use crate::model::{
    AirQuality, DayForecast, LocationId, SavedLocation, WeatherModel, WeatherSnapshot,
};
use crate::net::{Request, Response};
use crate::units::{self, Units};
use crate::wxcode::Sky;

const PAD: f64 = 28.0;
const RP: f64 = 20.0; // overview inner padding
const CHIP_H: f64 = 62.0; // saved-place chip height
const CHIP_GAP: f64 = 8.0;
const HERO_H: f64 = 168.0;
const TILE_H: f64 = 78.0;
const TILE_MIN_W: f64 = 118.0;
/// Height of the hourly-strip card (header + the fixed-height column band +
/// the horizontal scrollbar gutter).
const HOURLY_CARD_H: f64 = 190.0;
/// Minimum width of one hour column; below this the strip scrolls horizontally
/// rather than squeezing all 24 hours into the card width.
const HOUR_COL_MIN_W: f64 = 58.0;
/// Height of one air-quality pollutant tile.
const AQ_TILE_H: f64 = 66.0;
const DAY_ROW_H: f64 = 42.0;
const SECTION_GAP: f64 = 12.0;
/// Extra top padding reserved in the detail header for the shell's Back
/// affordance (chip + label) when the detail is pushed on a narrow layout.
const BACK_RESERVE: f64 = 30.0;
/// How long a recently-typed query stays "fresh" before we fire a geocode.
const GEOCODE_DEBOUNCE: Duration = Duration::from_millis(220);
/// Below this allocated width the overview renders as a collapsed **rail**
/// (glanceable icon column) instead of the full city list. The rail width
/// (see [`crate::app`]) and the side-by-side overview width sit either side
/// of this threshold, so the render mode follows the allocation with no
/// separate cached flag.
const RAIL_MAX_WIDTH: f64 = 120.0;
/// Height of one city slot in the collapsed rail.
const RAIL_SLOT_H: f64 = 62.0;

/// An intent the overview raises for the shell to act on (push the detail
/// in). Consumed by the root widget after routing an event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NavIntent {
    ShowDetail,
}

/// A click target in the overview, rebuilt each draw and hit-tested on input.
struct Hotspot {
    rect: Rect,
    action: OverAction,
}

#[derive(Clone)]
enum OverAction {
    SelectLocation(usize),
    MenuItem(usize, EntryAction),
    /// Begin a potential press on a chip (click / reorder / swipe).
    PressChip(usize),
    /// Collapse the full city list to a rail, or expand the rail back.
    ToggleCollapse,
}

/// All mutable weather state shared by the two panes and the root widget.
pub struct Core {
    pub model: WeatherModel,
    /// Display units — read from the shared Settings store (`region`), never
    /// toggled inside this app.
    pub units: Units,

    last_mouse: Point,

    // --- overview: saved-place direct manipulation ---
    /// Press classifier: distinguishes a click from a vertical reorder-drag
    /// from a horizontal delete-swipe once the pointer moves.
    press: Option<PointerPress>,
    menu: Option<ContextMenu>,
    /// The reusable drag-to-reorder model (live gap + reflow + floating
    /// ghost). Armed once a press classifies as a vertical reorder; commits
    /// the `(from, to)` move on drop and restores the order on cancel.
    reorder: DragController<LocationId>,
    over_rect: Rect,
    over_list_top: f64,
    over_scroll: f64,
    over_max_scroll: f64,
    over_hotspots: Vec<Hotspot>,

    // --- overview: search (shared Autocomplete widget) ---
    /// The shared type-ahead field + suggestion overlay. Owns the query text,
    /// caret / selection / clipboard, and the arrow-key / Enter / Esc
    /// navigation of the geocoded matches. Weather feeds it results reactively
    /// (the geocode is a debounced network side effect), never a synchronous
    /// suggester closure.
    search: Autocomplete,
    /// The field's on-screen rect (set each draw) so a press can begin an edit.
    search_field_rect: Rect,
    /// The last query text folded into the geocode machinery, for change
    /// detection against the field's live text.
    search_query: String,
    search_results: Vec<SavedLocation>,
    search_seq: u64,
    pending_query_at: Option<Instant>,
    geocode_cache: HashMap<String, Vec<SavedLocation>>,
    geocode_inflight: bool,

    // --- detail: forecast ---
    detail_rect: Rect,
    detail_reserve_back: bool,
    refresh_btn: Rect,
    scroll: f64,
    max_scroll: f64,

    // --- detail: the horizontally-scrollable 24h strip ---
    hourly_scroll_x: f64,
    hourly_max_scroll: f64,
    /// The strip's visible band (absolute coords, updated each draw).
    hourly_viewport: Rect,
    /// The strip's scrollbar track (absolute coords, updated each draw).
    hourly_track: Rect,
    /// Full width of all 24 columns (for the scrollbar metrics).
    hourly_content_w: f64,
    /// Shared scrollbar interaction model for the strip's horizontal axis.
    hourly_bar: Scrollbar,

    // --- network coordination (drained by the app's net pump) ---
    outbox: Vec<Request>,
    inflight: usize,
    pub status: String,

    // --- overview: collapse-to-rail (wide layout only) ---
    /// Whether the app currently offers the collapse affordance (set each
    /// frame by the root widget; true only in the side-by-side layout).
    overview_collapsible: bool,
    /// Raised when the collapse / expand chip is clicked; the root widget
    /// consumes it and rebuilds the shell at the new overview width.
    collapse_toggle: bool,

    // --- shell intent (consumed by the root widget) ---
    nav_request: Option<NavIntent>,

    pub pending_doc_write: bool,
}

impl Core {
    pub fn new(model: WeatherModel, units: Units) -> Self {
        Self {
            model,
            units,
            last_mouse: Point::ZERO,
            press: None,
            menu: None,
            reorder: DragController::vertical(),
            over_rect: Rect::ZERO,
            over_list_top: 0.0,
            over_scroll: 0.0,
            over_max_scroll: 0.0,
            over_hotspots: Vec::new(),
            search: Autocomplete::new()
                .with_id("weather.city_search")
                .with_placeholder("Search city…")
                .with_font_size(13.0),
            search_field_rect: Rect::ZERO,
            search_query: String::new(),
            search_results: Vec::new(),
            search_seq: 0,
            pending_query_at: None,
            geocode_cache: HashMap::new(),
            geocode_inflight: false,
            detail_rect: Rect::ZERO,
            detail_reserve_back: false,
            refresh_btn: Rect::ZERO,
            scroll: 0.0,
            max_scroll: 0.0,
            hourly_scroll_x: 0.0,
            hourly_max_scroll: 0.0,
            hourly_viewport: Rect::ZERO,
            hourly_track: Rect::ZERO,
            hourly_content_w: 0.0,
            hourly_bar: Scrollbar::horizontal(),
            outbox: Vec::new(),
            inflight: 0,
            status: String::new(),
            overview_collapsible: false,
            collapse_toggle: false,
            nav_request: None,
            pending_doc_write: false,
        }
    }

    // ---- app-facing coordination ------------------------------------

    /// Take the queued network requests (the app pumps them to the worker).
    pub fn take_outbox(&mut self) -> Vec<Request> {
        std::mem::take(&mut self.outbox)
    }

    /// Take a pending shell navigation intent, if any.
    pub fn take_nav_request(&mut self) -> Option<NavIntent> {
        self.nav_request.take()
    }

    /// Set whether the collapse affordance is offered (side-by-side only).
    /// Called once per frame by the root widget, which alone knows the layout.
    pub fn set_overview_collapsible(&mut self, v: bool) {
        self.overview_collapsible = v;
    }

    /// Take a pending collapse/expand toggle raised by the chip. The root
    /// widget rebuilds the shell at the new overview width when this is set.
    pub fn take_collapse_toggle(&mut self) -> bool {
        std::mem::take(&mut self.collapse_toggle)
    }

    /// Whether a fetch is currently in flight (drives frame pumping).
    pub fn is_fetching(&self) -> bool {
        self.inflight > 0
    }

    /// Whether a geocode is armed or in flight. The app keeps pumping frames
    /// while this holds so the result is drained and shown promptly (rather
    /// than the loop parking on "Searching…" until the next input event).
    pub fn search_pending(&self) -> bool {
        self.geocode_inflight || self.pending_query_at.is_some()
    }

    /// Whether a geocode debounce is armed (so the app keeps pumping frames
    /// until it fires).
    pub fn debounce_pending(&self) -> bool {
        self.pending_query_at.is_some()
    }

    /// Queue a forecast + air-quality fetch for every saved location.
    pub fn refresh_all(&mut self) {
        let units = self.units;
        let mut reqs: Vec<Request> = Vec::new();
        for loc in &self.model.locations {
            reqs.push(Request::Forecast {
                location_id: loc.id,
                lat: loc.latitude,
                lon: loc.longitude,
                units,
            });
            reqs.push(Request::AirQuality {
                location_id: loc.id,
                lat: loc.latitude,
                lon: loc.longitude,
            });
        }
        self.inflight += reqs.len();
        self.outbox.extend(reqs);
        if !self.model.locations.is_empty() {
            self.status = "Updating…".to_string();
        }
    }

    /// Open a saved city by id: select it, reset the forecast scroll, refresh
    /// its forecast, and raise the show-detail push intent — the same path a
    /// tap on the city list takes ([`OverAction::SelectLocation`]). Returns
    /// `false` (touching nothing) for an id that is not a saved city. Used by
    /// the app's Ctrl+K search activation.
    pub fn open_location(&mut self, id: LocationId) -> bool {
        let Some(pos) = self.model.locations.iter().position(|l| l.id == id) else {
            return false;
        };
        self.model.select(pos);
        self.scroll = 0.0;
        self.refresh_selected();
        self.nav_request = Some(NavIntent::ShowDetail);
        true
    }

    fn refresh_selected(&mut self) {
        if let Some(loc) = self.model.selected_location() {
            let (id, lat, lon) = (loc.id, loc.latitude, loc.longitude);
            self.outbox.push(Request::Forecast {
                location_id: id,
                lat,
                lon,
                units: self.units,
            });
            self.outbox.push(Request::AirQuality {
                location_id: id,
                lat,
                lon,
            });
            self.inflight += 2;
            self.status = "Updating…".to_string();
        }
    }

    /// Move the overview selection by `delta` (arrow-key navigation of the
    /// city list), re-fetch the newly selected city's forecast and reset the
    /// forecast scroll. Returns true when the selection actually moved, so the
    /// caller can persist the new `selected` to the shared document.
    fn move_selection(&mut self, delta: i64) -> bool {
        if self.model.locations.is_empty() {
            return false;
        }
        let before = self.model.selected;
        self.model.select_relative(delta);
        let moved = self.model.selected != before;
        if moved {
            self.scroll = 0.0;
            self.refresh_selected();
        }
        moved
    }

    fn refresh_location(&mut self, idx: usize) {
        if let Some(loc) = self.model.locations.get(idx) {
            let (id, lat, lon) = (loc.id, loc.latitude, loc.longitude);
            self.outbox.push(Request::Forecast {
                location_id: id,
                lat,
                lon,
                units: self.units,
            });
            self.outbox.push(Request::AirQuality {
                location_id: id,
                lat,
                lon,
            });
            self.inflight += 2;
            self.status = "Updating…".to_string();
        }
    }

    /// Apply a completed network response to the model / search results.
    pub fn apply_response(&mut self, resp: Response) {
        match resp {
            Response::Forecast(snap) => {
                self.inflight = self.inflight.saturating_sub(1);
                self.model.snapshots.insert(snap.location_id, snap);
                self.pending_doc_write = true;
                self.status = "Updated just now".to_string();
            }
            Response::AirQuality(aq) => {
                self.inflight = self.inflight.saturating_sub(1);
                self.model.air_quality.insert(aq.location_id, aq);
                self.pending_doc_write = true;
            }
            Response::Geocode { seq, results } => {
                if seq == self.search_seq {
                    self.geocode_inflight = false;
                    let key = self.search_query.trim().to_lowercase();
                    if !key.is_empty() {
                        self.geocode_cache.insert(key, results.clone());
                    }
                    // The dropdown surfaces match/no-match state; the forecast
                    // status line is reserved for the forecast itself.
                    self.search_results = results;
                    self.sync_search_view();
                }
            }
            Response::Error(e) => {
                self.inflight = self.inflight.saturating_sub(1);
                self.geocode_inflight = false;
                self.status = format!("Offline · {e}");
                // A failed geocode must resolve the dropdown's spinner into an
                // honest state rather than spin forever.
                self.sync_search_view();
            }
        }
    }

    /// Push the current search state into the shared [`Autocomplete`]: real
    /// suggestion rows when there are matches, else an honest loading or
    /// "no matches" status line — all inside the shared overlay.
    fn sync_search_view(&mut self) {
        if !self.search.is_editing() {
            return;
        }
        let q = self.search_query.trim().to_string();
        if q.is_empty() {
            self.search.set_suggestions(Vec::new());
            self.search.set_status(None);
            self.search.close();
            return;
        }
        if !self.search_results.is_empty() {
            let sugg: Vec<Suggestion> = self
                .search_results
                .iter()
                .map(|r| {
                    Suggestion::labelled(result_value(r), r.name.clone()).with_detail(r.subtitle())
                })
                .collect();
            self.search.set_suggestions(sugg);
            self.search.set_status(None);
        } else if self.search_pending() {
            self.search.set_suggestions(Vec::new());
            self.search.set_status(Some("Searching…".to_string()));
        } else {
            self.search.set_suggestions(Vec::new());
            self.search
                .set_status(Some(format!("No matches for “{q}”")));
        }
        self.search.open();
    }

    // ---- geocoding (debounced + cached) -----------------------------

    /// React to the search field's text changing: cache lookup for instant
    /// matches, arm the debounce, and refresh the dropdown's view.
    fn on_query_changed(&mut self, text: &str) {
        self.search_query = text.to_string();
        let key = text.trim().to_lowercase();
        if key.is_empty() {
            self.search_results.clear();
            self.pending_query_at = None;
            self.sync_search_view();
            return;
        }
        // Show cached matches instantly; otherwise the dropdown shows its
        // loading state until the debounced geocode lands.
        if let Some(hit) = self.geocode_cache.get(&key) {
            self.search_results = hit.clone();
        } else {
            self.search_results.clear();
        }
        self.pending_query_at = Some(Instant::now());
        self.sync_search_view();
    }

    /// If the debounce window elapsed, queue the geocode request now. Called
    /// once per frame by the app.
    pub fn maybe_fire_geocode(&mut self) {
        let Some(at) = self.pending_query_at else {
            return;
        };
        if at.elapsed() < GEOCODE_DEBOUNCE {
            return;
        }
        self.pending_query_at = None;
        let q = self.search_query.trim().to_string();
        if q.is_empty() {
            return;
        }
        let key = q.to_lowercase();
        if self.geocode_cache.contains_key(&key) {
            return; // already shown from cache
        }
        self.search_seq += 1;
        self.geocode_inflight = true;
        self.outbox.push(Request::Geocode {
            seq: self.search_seq,
            query: q,
        });
        // The spinner in the dropdown reflects the now-in-flight geocode.
        self.sync_search_view();
    }

    // ---- search lifecycle (begin / commit / end) --------------------

    /// Focus the shared search field and open an empty edit. Called from a
    /// press on the field or the `/` shortcut.
    fn begin_search(&mut self) {
        self.search.set_text("");
        self.search.begin_edit();
        self.search_query.clear();
        self.search_results.clear();
        self.pending_query_at = None;
        self.geocode_inflight = false;
    }

    /// Blur the search field and clear its transient state.
    fn end_search(&mut self) {
        self.search.close();
        self.search.set_focus(false);
        self.search.set_text("");
        self.search_query.clear();
        self.search_results.clear();
        self.pending_query_at = None;
        self.geocode_inflight = false;
    }

    /// Resolve an accepted autocomplete value back to a geocoded place and add
    /// it. The encoded "Name, Region, Country" value identifies the exact match
    /// (so arrowing to the second "Paris" adds the right one); a bare typed
    /// query (Enter with no highlight) falls back to the top match. Returns
    /// true when a place was actually added.
    fn commit_search_pick(&mut self, value: String) -> bool {
        let picked = self
            .search_results
            .iter()
            .find(|r| result_value(r) == value)
            .cloned()
            .or_else(|| self.search_results.first().cloned());
        if let Some(loc) = picked {
            self.model.add_location(loc);
            self.scroll = 0.0;
            self.refresh_selected();
            self.nav_request = Some(NavIntent::ShowDetail);
            self.pending_doc_write = true;
            return true;
        }
        // Enter while still loading: arm an immediate geocode so the field
        // resolves to real matches rather than doing nothing.
        if !self.search_query.trim().is_empty() {
            self.pending_query_at = Some(Instant::now().checked_sub(GEOCODE_DEBOUNCE).unwrap());
        }
        false
    }

    // ---- per-entry actions (swipe ⇔ menu parity) --------------------

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
                false
            }
        }
    }

    // ---- shared visual helpers --------------------------------------

    fn accent(&self, theme: &Theme) -> Color {
        let day = self.is_day();
        if day {
            theme.tokens.colors.accent[5]
        } else {
            theme.tokens.colors.accent[6]
        }
    }

    fn is_day(&self) -> bool {
        self.model
            .selected_snapshot()
            .is_none_or(|s| s.current.is_day)
    }

    // =================================================================
    // OVERVIEW pane — the contentful city list (master)
    // =================================================================

    /// Set the detail pane's reserved top space for the shell's Back chip.
    /// Called by the app before drawing (only the shell knows the affordance).
    pub fn set_detail_reserve_back(&mut self, reserve: bool) {
        self.detail_reserve_back = reserve;
    }

    pub fn draw_overview(
        &mut self,
        rect: Rect,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        self.over_rect = rect;
        // Below the rail threshold the overview collapses to a glanceable
        // icon column. The mode is re-derived from the allocated width every
        // frame, so a resize between side-by-side and rail reflows on its own.
        if rect.width() < RAIL_MAX_WIDTH {
            self.draw_overview_rail(rect, scene, t, tr, theme);
            return;
        }

        let c = &theme.tokens.colors;
        let mut hot: Vec<Hotspot> = Vec::new();

        // Solid surface — this is a content panel, not a floating drawer.
        scene.fill(Fill::NonZero, t, c.surfaces.base, None, &rect);

        let x = rect.x0 + RP;
        let w = (rect.width() - 2.0 * RP).max(80.0);

        // The compositor owns the window title / chrome, so this panel opens
        // straight into its content: the search field, then the saved places.
        // No self-drawn app-name header.
        let sy = rect.y0 + 16.0;
        let sh = 38.0;
        let mut field_w = w;

        // Collapse chip (side-by-side only) — tucks the list into a rail so
        // the forecast can use the full width. It sits at the search field's
        // right edge, which the field leaves room for.
        if self.overview_collapsible {
            let cb = 30.0;
            let cy = sy + (sh - cb) / 2.0;
            let chip = Rect::new(x + w - cb, cy, x + w, cy + cb);
            let hovered = chip.contains(self.last_mouse);
            let shape = RoundedRect::from_rect(chip, f64::from(theme.tokens.radii.md));
            scene.fill(
                Fill::NonZero,
                t,
                if hovered {
                    c.neutral[2]
                } else {
                    c.surfaces.raised
                },
                None,
                &shape,
            );
            scene.stroke(&Stroke::new(1.0), t, c.border_standard, None, &shape);
            Icon::ChevronLeft.draw(scene, t, chip.inset(-8.0), c.text_primary);
            hot.push(Hotspot {
                rect: chip,
                action: OverAction::ToggleCollapse,
            });
            field_w = (w - cb - 10.0).max(80.0);
        }

        // Search field — the shared Autocomplete (a real text field, its
        // suggestion overlay floats last). No hand-rolled buffer / caret / hit
        // rect here; the widget owns selection, clipboard, IME and theming.
        let field_rect = Rect::new(x, sy, x + field_w, sy + sh);
        self.search_field_rect = field_rect;
        self.search.apply_layout_external(field_rect);
        let presence = PresenceCtx::empty();
        self.search.draw(scene, t, tr, theme, &presence);

        // The saved-place list keeps a fixed position — the live search
        // matches render as a floating dropdown over it (drawn last), so the
        // list never jumps around while typing.
        let mut y = sy + sh + 16.0;

        text_left(
            scene,
            tr,
            "SAVED PLACES",
            x,
            y,
            10.0,
            TextWeight::Semibold,
            c.text_dim,
        );
        y += 22.0;

        // The chip list scrolls within the region below the header. While a
        // reorder is in flight the foot shows the Delete drop zone.
        let list_top = y;
        let reorder = self.is_reorder_active();
        let list_bottom = if reorder {
            rect.y1 - 96.0 - 2.0 * RP
        } else {
            rect.y1 - RP
        };
        let view_h = (list_bottom - list_top).max(0.0);
        let n = self.model.locations.len();
        let content_h = n as f64 * (CHIP_H + CHIP_GAP);
        self.over_max_scroll = (content_h - view_h).max(0.0);
        self.over_scroll = self.over_scroll.clamp(0.0, self.over_max_scroll);
        self.over_list_top = list_top - self.over_scroll;

        // Re-pin the live insertion gap to the cursor against this frame's
        // geometry so a held drag reflows correctly even as the list scrolls.
        let metrics = self.list_metrics();
        self.reorder.sync_insertion(&metrics);

        // Clip the scrolling list to its region.
        let clip = Rect::new(rect.x0, list_top - 2.0, rect.x1, list_bottom + 2.0);
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &clip);
        self.draw_chips(scene, t, tr, theme, x, w, &mut hot);
        scene.pop_layer();

        if self.model.locations.is_empty() {
            text_left(
                scene,
                tr,
                "No places yet — search above",
                x,
                list_top + 8.0,
                13.0,
                TextWeight::Regular,
                c.text_dim,
            );
        }

        // Foot: Delete drop zone while reordering.
        if reorder {
            self.draw_delete_dropzone(scene, t, tr, theme, rect);
        }

        // Scroll indicator.
        if self.over_max_scroll > 1.0 {
            let tx = rect.x1 - 6.0;
            let track_h = list_bottom - list_top;
            let thumb_h = (track_h * (view_h / content_h)).clamp(24.0, track_h);
            let thumb_y =
                list_top + (self.over_scroll / self.over_max_scroll) * (track_h - thumb_h);
            scene.fill(
                Fill::NonZero,
                t,
                c.neutral[2].with_alpha_factor(0.6),
                None,
                &RoundedRect::new(tx, list_top, tx + 3.0, list_bottom, 1.5),
            );
            scene.fill(
                Fill::NonZero,
                t,
                c.text_dim.with_alpha_factor(0.7),
                None,
                &RoundedRect::new(tx, thumb_y, tx + 3.0, thumb_y + thumb_h, 1.5),
            );
        }

        // Context menu paints last so it sits above the chips.
        if self.menu.is_some() {
            self.draw_context_menu(scene, t, tr, theme, &mut hot);
        }

        // The shared suggestion overlay floats over the list, topmost.
        if self.search.is_editing() {
            self.search.draw_overlay(scene, t, tr, theme, &presence);
        }

        self.over_hotspots = hot;
    }

    /// The collapsed **rail**: a slim, glanceable column of the saved cities
    /// (condition icon + temperature) plus an expand chip at the top. Clicking
    /// a city selects it; clicking the chip restores the full list. Rendered
    /// whenever the overview is allocated less than [`RAIL_MAX_WIDTH`].
    fn draw_overview_rail(
        &mut self,
        rect: Rect,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let c = &theme.tokens.colors;
        let mut hot: Vec<Hotspot> = Vec::new();
        scene.fill(Fill::NonZero, t, c.surfaces.base, None, &rect);
        // The rail has no search field; drop any stale rect so a press can't
        // land on a phantom field from the previous (wide) layout.
        self.search_field_rect = Rect::ZERO;
        let cx = f64::midpoint(rect.x0, rect.x1);

        // Expand chip.
        let cs = 36.0;
        let chip = Rect::new(
            cx - cs / 2.0,
            rect.y0 + 12.0,
            cx + cs / 2.0,
            rect.y0 + 12.0 + cs,
        );
        let hovered = chip.contains(self.last_mouse);
        let shape = RoundedRect::from_rect(chip, f64::from(theme.tokens.radii.md));
        scene.fill(
            Fill::NonZero,
            t,
            if hovered {
                c.neutral[2]
            } else {
                c.surfaces.raised
            },
            None,
            &shape,
        );
        scene.stroke(&Stroke::new(1.0), t, c.border_standard, None, &shape);
        Icon::ChevronRight.draw(scene, t, chip.inset(-8.0), c.text_primary);
        hot.push(Hotspot {
            rect: chip,
            action: OverAction::ToggleCollapse,
        });

        // Separator under the chip.
        let sep_y = chip.y1 + 12.0;
        scene.fill(
            Fill::NonZero,
            t,
            c.border_standard.with_alpha_factor(0.6),
            None,
            &Rect::new(rect.x0 + 10.0, sep_y, rect.x1 - 10.0, sep_y + 1.0),
        );

        // Scrolling city column.
        let list_top = sep_y + 12.0;
        let list_bottom = rect.y1 - 10.0;
        let view_h = (list_bottom - list_top).max(0.0);
        let n = self.model.locations.len();
        let content_h = n as f64 * RAIL_SLOT_H;
        self.over_max_scroll = (content_h - view_h).max(0.0);
        self.over_scroll = self.over_scroll.clamp(0.0, self.over_max_scroll);
        let top = list_top - self.over_scroll;
        self.over_list_top = top;

        let clip = Rect::new(rect.x0, list_top - 2.0, rect.x1, list_bottom + 2.0);
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &clip);
        for (i, loc) in self.model.locations.iter().enumerate() {
            let sy = top + i as f64 * RAIL_SLOT_H;
            let slot = Rect::new(
                rect.x0 + 6.0,
                sy + 4.0,
                rect.x1 - 6.0,
                sy + RAIL_SLOT_H - 6.0,
            );
            let selected = i == self.model.selected;
            let hov = slot.contains(self.last_mouse);
            if selected {
                let bg = lerp_color(c.surfaces.base, self.accent(theme), 0.18);
                scene.fill(
                    Fill::NonZero,
                    t,
                    bg,
                    None,
                    &RoundedRect::from_rect(slot, 8.0),
                );
                scene.fill(
                    Fill::NonZero,
                    t,
                    self.accent(theme),
                    None,
                    &RoundedRect::new(slot.x0, slot.y0 + 6.0, slot.x0 + 3.0, slot.y1 - 6.0, 1.5),
                );
            } else if hov {
                scene.fill(
                    Fill::NonZero,
                    t,
                    c.neutral[2],
                    None,
                    &RoundedRect::from_rect(slot, 8.0),
                );
            }
            if let Some(snap) = self.model.snapshot_for(loc.id) {
                let sky = Sky::from_code(snap.current.weather_code);
                icon::draw_icon(
                    scene,
                    t,
                    sky,
                    snap.current.is_day,
                    Rect::new(cx - 15.0, slot.y0 + 4.0, cx + 15.0, slot.y0 + 34.0),
                    theme,
                );
                let temp = snap.temp_in(snap.current.temperature, self.units).round() as i64;
                text_center(
                    scene,
                    tr,
                    &format!("{temp}°"),
                    cx,
                    slot.y1 - 18.0,
                    14.0,
                    TextWeight::Bold,
                    c.text_primary,
                );
            } else {
                text_center(
                    scene,
                    tr,
                    "—",
                    cx,
                    slot.y0 + 14.0,
                    15.0,
                    TextWeight::Regular,
                    c.text_dim,
                );
            }
            hot.push(Hotspot {
                rect: slot,
                action: OverAction::SelectLocation(i),
            });
        }
        scene.pop_layer();

        if self.over_max_scroll > 1.0 {
            let tx = rect.x1 - 4.0;
            let track_h = list_bottom - list_top;
            let thumb_h = (track_h * (view_h / content_h)).clamp(20.0, track_h);
            let thumb_y =
                list_top + (self.over_scroll / self.over_max_scroll) * (track_h - thumb_h);
            scene.fill(
                Fill::NonZero,
                t,
                c.text_dim.with_alpha_factor(0.6),
                None,
                &RoundedRect::new(tx, thumb_y, tx + 2.5, thumb_y + thumb_h, 1.25),
            );
        }

        self.over_hotspots = hot;
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_chips(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        x: f64,
        w: f64,
        hot: &mut Vec<Hotspot>,
    ) {
        let units = self.units;
        let metrics = self.list_metrics();
        let dragging = self.reorder.from_index();
        let slot_y = |i: usize| self.over_list_top + i as f64 * (CHIP_H + CHIP_GAP);

        // While a reorder is live the toolkit's `DragController` reflows the
        // list: every non-dragged chip slides by `item_shift` to close the
        // vacated hole and open a one-slot gap exactly where the dragged chip
        // will land — so the list visibly rearranges under the cursor rather
        // than only on release. A faint accent caret sits at the top of the
        // reflowed gap (the live target slot) to mark where the chip will drop.
        if let Some(gap) = self.reorder.insertion() {
            let gy = slot_y(gap) - CHIP_GAP / 2.0;
            scene.fill(
                Fill::NonZero,
                t,
                self.accent(theme),
                None,
                &RoundedRect::new(x, gy - 1.5, x + w, gy + 1.5, 1.5),
            );
        }

        for (i, loc) in self.model.locations.iter().enumerate() {
            if Some(i) == dragging {
                continue;
            }
            let y = slot_y(i) + self.reorder.item_shift(i, &metrics);
            let chip = Rect::new(x, y, x + w, y + CHIP_H);
            let swipe_dx = self.swipe_dx_for(i);
            if swipe_dx > 0.0 {
                self.draw_swipe_backing(scene, t, theme, chip);
            }
            let chip_t = t * Affine::translate((-swipe_dx, 0.0));
            self.draw_chip(scene, chip_t, tr, theme, chip, loc, i, units);
            hot.push(Hotspot {
                rect: chip,
                action: OverAction::PressChip(i),
            });
        }

        // The dragged chip floats free as a ghost at its grab offset under the
        // cursor, drawn last so it rides above the reflowing list.
        if let Some(i) = dragging
            && let (Some(loc), Some(chip)) = (
                self.model.locations.get(i),
                self.reorder.ghost_rect(x, x + w, &metrics),
            )
        {
            let sh =
                RoundedRect::from_rect(chip.inset(-2.0), f64::from(theme.tokens.radii.md) + 2.0);
            scene.fill(Fill::NonZero, t, Color::from_rgba8(0, 0, 0, 60), None, &sh);
            self.draw_chip(scene, t, tr, theme, chip, loc, i, units);
        }
    }

    /// The reorderable saved-place list's geometry this frame, for the
    /// toolkit's [`DragController`]. `over_list_top` is the scroll-adjusted
    /// leading edge of the first chip, set during `draw_overview`.
    fn list_metrics(&self) -> ListMetrics {
        ListMetrics::uniform(
            self.over_list_top,
            CHIP_H,
            CHIP_GAP,
            self.model.locations.len(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_chip(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        chip: Rect,
        loc: &SavedLocation,
        index: usize,
        units: Units,
    ) {
        let c = &theme.tokens.colors;
        let selected = index == self.model.selected;
        let hovered = chip.contains(self.last_mouse);
        let bg = if selected {
            lerp_color(c.surfaces.base, self.accent(theme), 0.18)
        } else if hovered {
            c.neutral[2]
        } else {
            c.surfaces.raised
        };
        let shape = RoundedRect::from_rect(chip, f64::from(theme.tokens.radii.md));
        scene.fill(Fill::NonZero, t, bg, None, &shape);
        if selected {
            scene.stroke(&Stroke::new(1.5), t, self.accent(theme), None, &shape);
        } else {
            scene.stroke(
                &Stroke::new(1.0),
                t,
                c.border_standard.with_alpha_factor(0.6),
                None,
                &shape,
            );
        }
        if let Some(snap) = self.model.snapshot_for(loc.id) {
            let sky = Sky::from_code(snap.current.weather_code);
            icon::draw_icon(
                scene,
                t,
                sky,
                snap.current.is_day,
                Rect::new(
                    chip.x0 + 10.0,
                    chip.y0 + 11.0,
                    chip.x0 + 50.0,
                    chip.y0 + 51.0,
                ),
                theme,
            );
        }
        text_left(
            scene,
            tr,
            &loc.name,
            chip.x0 + 58.0,
            chip.y0 + 12.0,
            15.0,
            TextWeight::Semibold,
            c.text_primary,
        );
        // Second line: country + condition word (the "quick per-city info").
        let sub = if let Some(snap) = self.model.snapshot_for(loc.id) {
            format!(
                "{} · {}",
                loc.country,
                Sky::label(snap.current.weather_code)
            )
        } else {
            loc.country.clone()
        };
        text_left(
            scene,
            tr,
            &sub,
            chip.x0 + 58.0,
            chip.y0 + 35.0,
            12.0,
            TextWeight::Regular,
            c.text_dim,
        );
        if let Some(snap) = self.model.snapshot_for(loc.id) {
            let temp = snap.temp_in(snap.current.temperature, units).round() as i64;
            text_right(
                scene,
                tr,
                &format!("{temp}°"),
                chip.x1 - 14.0,
                chip.y0 + 16.0,
                24.0,
                TextWeight::Bold,
                c.text_primary,
            );
        } else {
            text_right(
                scene,
                tr,
                "—",
                chip.x1 - 16.0,
                chip.y0 + 20.0,
                18.0,
                TextWeight::Regular,
                c.text_dim,
            );
        }
    }

    fn draw_swipe_backing(&self, scene: &mut Scene, t: Affine, theme: &Theme, chip: Rect) {
        let c = &theme.tokens.colors;
        let shape = RoundedRect::from_rect(chip, f64::from(theme.tokens.radii.md));
        scene.fill(Fill::NonZero, t, c.semantic.danger, None, &shape);
        let icon_r = Rect::new(
            chip.x1 - 42.0,
            chip.y0 + 17.0,
            chip.x1 - 14.0,
            chip.y0 + 45.0,
        );
        Icon::Trash.draw(scene, t, icon_r, Color::WHITE);
    }

    fn draw_delete_dropzone(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
    ) {
        let c = &theme.tokens.colors;
        let zone = self.delete_zone_rect();
        let w = (rect.width() - 2.0 * RP).max(80.0);
        let armed = self.is_reorder_active() && zone.contains(self.last_mouse);
        let shape = RoundedRect::from_rect(zone, f64::from(theme.tokens.radii.lg));
        let fill = if armed {
            c.semantic.danger
        } else {
            c.semantic.danger.with_alpha_factor(0.16)
        };
        scene.fill(Fill::NonZero, t, fill, None, &shape);
        let dash = Stroke::new(2.0).with_dashes(0.0, [6.0, 5.0]);
        scene.stroke(&dash, t, c.semantic.danger, None, &shape);
        let icon_color = if armed {
            Color::WHITE
        } else {
            c.semantic.danger
        };
        let icon_r = Rect::new(
            zone.x0 + w / 2.0 - 16.0,
            zone.y0 + 14.0,
            zone.x0 + w / 2.0 + 16.0,
            zone.y0 + 46.0,
        );
        Icon::Trash.draw_with_stroke(scene, t, icon_r, icon_color, 2.0);
        let label_color = if armed {
            Color::WHITE
        } else {
            c.semantic.danger
        };
        text_center(
            scene,
            tr,
            "Delete",
            zone.x0 + w / 2.0,
            zone.y1 - 26.0,
            14.0,
            TextWeight::Bold,
            label_color,
        );
    }

    fn draw_context_menu(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        hot: &mut Vec<Hotspot>,
    ) {
        let Some(menu) = self.menu.as_ref() else {
            return;
        };
        let c = &theme.tokens.colors;
        let mw = 184.0;
        let row_h = 34.0;
        let pad = 6.0;
        let mh = pad * 2.0 + menu.items.len() as f64 * row_h;
        let area = self.over_rect;
        let mx = menu.anchor.x.min(area.x1 - mw - 4.0).max(area.x0 + 4.0);
        let my = menu.anchor.y.min(area.y1 - mh - 4.0).max(area.y0 + 4.0);
        let panel = Rect::new(mx, my, mx + mw, my + mh);
        let shape = RoundedRect::from_rect(panel, f64::from(theme.tokens.radii.md));
        scene.fill(
            Fill::NonZero,
            t,
            Color::from_rgba8(0, 0, 0, 80),
            None,
            &RoundedRect::from_rect(panel.inset(-3.0), f64::from(theme.tokens.radii.md) + 3.0),
        );
        scene.fill(
            Fill::NonZero,
            t,
            c.surfaces.overlay.with_alpha_factor(0.98),
            None,
            &shape,
        );
        scene.stroke(&Stroke::new(1.0), t, c.border_standard, None, &shape);
        for (i, action) in menu.items.iter().enumerate() {
            let ry = my + pad + i as f64 * row_h;
            let row = Rect::new(mx + pad, ry, mx + mw - pad, ry + row_h);
            if row.contains(self.last_mouse) {
                let hl = if action.is_destructive() {
                    c.semantic.danger.with_alpha_factor(0.22)
                } else {
                    c.neutral[2]
                };
                scene.fill(
                    Fill::NonZero,
                    t,
                    hl,
                    None,
                    &RoundedRect::from_rect(row, 6.0),
                );
            }
            let text_color = if action.is_destructive() {
                c.semantic.danger
            } else {
                c.text_primary
            };
            let icon_r = Rect::new(row.x0 + 6.0, row.y0 + 8.0, row.x0 + 24.0, row.y0 + 26.0);
            match action {
                EntryAction::Delete => Icon::Trash.draw(scene, t, icon_r, text_color),
                EntryAction::MoveToTop => Icon::ChevronUp.draw(scene, t, icon_r, text_color),
                EntryAction::Refresh => Icon::Refresh.draw(scene, t, icon_r, text_color),
            }
            text_left(
                scene,
                tr,
                action.label(),
                row.x0 + 32.0,
                row.y0 + row_h / 2.0 - 8.0,
                13.0,
                TextWeight::Medium,
                text_color,
            );
            hot.push(Hotspot {
                rect: row,
                action: OverAction::MenuItem(menu.index, *action),
            });
        }
    }

    // ---- overview drag / swipe helpers ------------------------------

    fn is_reorder_active(&self) -> bool {
        self.reorder.is_active()
    }

    fn swipe_dx_for(&self, i: usize) -> f64 {
        match self.press {
            Some(p) if p.is_swipe() && p.index == i => p.swipe_dx(),
            _ => 0.0,
        }
    }

    fn delete_zone_rect(&self) -> Rect {
        let rect = self.over_rect;
        let x = rect.x0 + RP;
        let w = (rect.width() - 2.0 * RP).max(80.0);
        let h = 96.0;
        Rect::new(x, rect.y1 - h - RP, x + w, rect.y1 - RP)
    }

    fn chip_at(&self, p: Point) -> Option<usize> {
        let rect = self.over_rect;
        let x = rect.x0 + RP;
        let w = (rect.width() - 2.0 * RP).max(80.0);
        for i in 0..self.model.locations.len() {
            let y = self.over_list_top + i as f64 * (CHIP_H + CHIP_GAP);
            if Rect::new(x, y, x + w, y + CHIP_H).contains(p) {
                return Some(i);
            }
        }
        None
    }

    fn apply_over_action(&mut self, action: OverAction) -> bool {
        match action {
            OverAction::SelectLocation(i) => {
                self.model.select(i);
                self.scroll = 0.0;
                self.refresh_selected();
                self.nav_request = Some(NavIntent::ShowDetail);
                true
            }
            OverAction::MenuItem(index, action) => {
                self.menu = None;
                self.apply_entry_action(index, action)
            }
            OverAction::ToggleCollapse => {
                // Raise the intent; the root widget rebuilds the shell at the
                // new overview width. Not a document mutation.
                self.collapse_toggle = true;
                false
            }
            OverAction::PressChip(_) => false,
        }
    }

    /// Handle an event routed to the overview pane. Returns true if consumed.
    #[allow(clippy::too_many_arguments)]
    pub fn handle_overview_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) -> bool {
        use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta};
        use winit::keyboard::{Key, NamedKey};

        // Keep the shared field placed so its overlay hit-tests correctly.
        self.search.apply_layout_external(self.search_field_rect);

        // A press over the field while not editing begins an edit.
        if let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: MouseButton::Left,
            ..
        } = event
            && !self.search.is_editing()
            && self.search_field_rect.contains(self.last_mouse)
        {
            self.begin_search();
            return true;
        }

        // While editing, the shared Autocomplete owns typing + arrow/Enter/Esc
        // navigation of the geocoded matches.
        if self.search.is_editing() {
            let before = self.search.text().to_string();
            let mut ac_handled = false;
            self.search.handle_event(
                event,
                scale_factor,
                modifiers,
                state,
                path,
                &mut ac_handled,
                focus_target,
                node_id,
                taffy,
            );
            match self.search.take_outcome() {
                AutocompleteOutcome::Accepted(value) => {
                    if self.commit_search_pick(value) {
                        self.end_search();
                    } else {
                        // Nothing to pick yet (Enter while loading): keep the
                        // edit open and show the honest loading state.
                        self.search.open();
                        self.sync_search_view();
                    }
                    return true;
                }
                AutocompleteOutcome::Cancelled => {
                    // A click-outside cancel is not consumed — fall through so
                    // the same press can still engage a chip beneath the field.
                    self.end_search();
                }
                AutocompleteOutcome::None => {}
            }
            let after = self.search.text().to_string();
            let text_changed = after != before;
            if text_changed {
                self.on_query_changed(&after);
            }
            // Consume anything the field acted on — a handled key/click or a
            // text edit — so a keystroke never leaks past the focused field.
            if ac_handled || text_changed {
                return true;
            }
        }

        let mut handled = false;
        let mut changed = false;

        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_mouse = Point::new(position.x / scale_factor, position.y / scale_factor);
                if let Some(press) = self.press.as_mut() {
                    press.update(self.last_mouse);
                }
                // Drive the reusable reorder model only while the gesture is a
                // vertical reorder. A horizontal delete-swipe drops any pending
                // drag so the two gestures never fight for the same pointer.
                if let Some(press) = self.press {
                    if press.is_reorder() {
                        let metrics = self.list_metrics();
                        if !self.reorder.is_pending() {
                            // Lazily arm the drag the moment it classifies as a
                            // reorder, grabbing the chip at the press point.
                            let grab = press.origin.y - metrics.slot_lead(press.index);
                            let id = self.model.locations[press.index].id;
                            self.reorder.begin(id, press.index, grab, press.origin);
                        }
                        self.reorder.pointer_moved(self.last_mouse, &metrics);
                    } else if press.is_swipe() {
                        self.reorder.cancel();
                    }
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                if self.over_rect.contains(self.last_mouse) && self.over_max_scroll > 0.0 {
                    let dy = match delta {
                        MouseScrollDelta::LineDelta(_, y) => f64::from(*y) * 48.0,
                        MouseScrollDelta::PixelDelta(p) => p.y / scale_factor,
                    };
                    self.over_scroll = (self.over_scroll - dy).clamp(0.0, self.over_max_scroll);
                    handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Right,
                ..
            } => {
                if let Some(idx) = self.chip_at(self.last_mouse) {
                    self.menu = Some(ContextMenu::for_entry(idx, self.last_mouse));
                    handled = true;
                } else if self.menu.is_some() {
                    self.menu = None;
                    handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if self.menu.is_some() {
                    let hit = self
                        .over_hotspots
                        .iter()
                        .rev()
                        .find(|h| {
                            matches!(h.action, OverAction::MenuItem(..))
                                && h.rect.contains(self.last_mouse)
                        })
                        .map(|h| h.action.clone());
                    if let Some(action) = hit {
                        changed = self.apply_over_action(action);
                    } else {
                        self.menu = None;
                    }
                    handled = true;
                } else {
                    let hit = self
                        .over_hotspots
                        .iter()
                        .rev()
                        .find(|h| h.rect.contains(self.last_mouse))
                        .map(|h| h.action.clone());
                    match hit {
                        Some(OverAction::PressChip(i)) => {
                            self.press = Some(PointerPress::new(i, self.last_mouse));
                            self.reorder.cancel();
                            handled = true;
                        }
                        Some(action) => {
                            changed = self.apply_over_action(action);
                            handled = true;
                        }
                        // A press on empty list space (the search field's own
                        // click-outside blur is handled by the Autocomplete
                        // routing above).
                        None => {}
                    }
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                if let Some(press) = self.press.take() {
                    handled = true;
                    if press.is_reorder() {
                        if self.delete_zone_rect().contains(self.last_mouse) {
                            // Drop onto the trash: abandon the reorder, delete.
                            self.reorder.cancel();
                            changed = self.apply_entry_action(press.index, EntryAction::Delete);
                        } else if let Some((from, to)) = self.reorder.commit() {
                            // Commit the live insertion the controller tracked.
                            changed = self.model.move_location(from, to);
                        } else {
                            // Dropped in place (or never activated): no change.
                            self.reorder.cancel();
                        }
                    } else if press.is_swipe() {
                        if press.swipe_dx() >= SWIPE_COMMIT_DX {
                            changed =
                                self.apply_entry_action(press.index, EntryAction::swipe_left());
                        }
                    } else {
                        changed = self.apply_over_action(OverAction::SelectLocation(press.index));
                    }
                }
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => {
                // Typing / arrow / Enter / Esc while the search field is
                // focused is owned by the shared Autocomplete above; this arm
                // only runs when the list itself has focus.
                match logical_key {
                    Key::Named(NamedKey::Escape) => {
                        // Escape abandons an in-flight reorder, snapping the
                        // list back to its original order (the model was
                        // never touched — the drag is purely visual until
                        // drop).
                        if self.reorder.cancel() {
                            self.press = None;
                            handled = true;
                        } else if self.menu.is_some() {
                            self.menu = None;
                            handled = true;
                        }
                    }
                    // Arrow up/down move the highlighted city; the detail
                    // pane follows the selection live (side-by-side) and a
                    // fresh forecast is fetched for it.
                    Key::Named(NamedKey::ArrowDown) => {
                        self.menu = None;
                        changed = self.move_selection(1);
                        handled = true;
                    }
                    Key::Named(NamedKey::ArrowUp) => {
                        self.menu = None;
                        changed = self.move_selection(-1);
                        handled = true;
                    }
                    // Enter opens the forecast for the highlighted city —
                    // the only way to reach the detail by keyboard in the
                    // narrow push layout, where it isn't shown yet.
                    Key::Named(NamedKey::Enter) => {
                        if self.model.selected_location().is_some() {
                            self.refresh_selected();
                            self.nav_request = Some(NavIntent::ShowDetail);
                            handled = true;
                        }
                    }
                    Key::Character(ch) if ch.as_str() == "/" => {
                        self.begin_search();
                        handled = true;
                    }
                    _ => {}
                }
            }
            _ => {}
        }

        if changed {
            self.pending_doc_write = true;
        }
        handled
    }

    // =================================================================
    // DETAIL pane — the forecast for the selected city
    // =================================================================

    pub fn draw_detail(
        &mut self,
        rect: Rect,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        self.detail_rect = rect;
        let c = &theme.tokens.colors;
        self.draw_sky_background(scene, t, theme, rect);

        let top_pad = if self.detail_reserve_back {
            PAD + BACK_RESERVE
        } else {
            PAD
        };
        let mx = rect.x0 + PAD;
        let mw = (rect.width() - 2.0 * PAD).max(140.0);
        let right = rect.x1 - PAD;
        let accent = self.accent(theme);

        let Some(loc) = self.model.selected_location().cloned() else {
            // Composed zero-state: a friendly weather glyph over an inviting
            // prompt, centred in the detail pane — never a bare label on a big
            // empty gradient (HIG §7). The copy differs by whether the user has
            // any saved places: a fresh vault points at the search; an existing
            // roster asks them to pick a city.
            let cx = rect.center().x;
            let anchor = rect.y0 + rect.height() * 0.34;
            let glyph = 92.0;
            icon::draw_icon(
                scene,
                t,
                Sky::PartlyCloudy,
                true,
                Rect::new(cx - glyph / 2.0, anchor, cx + glyph / 2.0, anchor + glyph),
                theme,
            );
            let (headline, sub) = if self.model.locations.is_empty() {
                (
                    "No cities yet",
                    "Search for a city to see its current conditions and 7-day forecast.",
                )
            } else {
                (
                    "Pick a city",
                    "Choose a saved place on the left to see its forecast.",
                )
            };
            text_center(
                scene,
                tr,
                headline,
                cx,
                anchor + glyph + 20.0,
                22.0,
                TextWeight::Bold,
                c.text_primary,
            );
            text_center(
                scene,
                tr,
                sub,
                cx,
                anchor + glyph + 54.0,
                14.0,
                TextWeight::Regular,
                c.text_dim,
            );
            self.refresh_btn = Rect::ZERO;
            self.max_scroll = 0.0;
            return;
        };
        let snap_opt = self.model.selected_snapshot().cloned();

        // ---- Pinned header ----
        let hy = rect.y0 + top_pad;
        text_left(
            scene,
            tr,
            &loc.name,
            mx,
            hy,
            26.0,
            TextWeight::Bold,
            c.text_primary,
        );
        text_left(
            scene,
            tr,
            &loc.subtitle(),
            mx,
            hy + 32.0,
            13.0,
            TextWeight::Regular,
            c.text_dim,
        );

        // Refresh button (top-right) — re-fetches this city's forecast.
        let rb = 34.0;
        let refresh = Rect::new(right - rb, hy - 2.0, right, hy - 2.0 + rb);
        let rshape = RoundedRect::from_rect(refresh, f64::from(theme.tokens.radii.md));
        let rhover = refresh.contains(self.last_mouse);
        scene.fill(
            Fill::NonZero,
            t,
            if rhover {
                c.neutral[2]
            } else {
                c.surfaces.raised.with_alpha_factor(0.9)
            },
            None,
            &rshape,
        );
        scene.stroke(&Stroke::new(1.0), t, c.border_standard, None, &rshape);
        Icon::Refresh.draw(scene, t, refresh.inset(-8.0), c.text_primary);
        self.refresh_btn = refresh;

        if let Some(snap) = &snap_opt {
            let cur = &snap.current;
            let date_str = fmt_date(&cur.time);
            text_right(
                scene,
                tr,
                &date_str,
                right - rb - 12.0,
                hy + 2.0,
                14.0,
                TextWeight::Medium,
                c.text_primary,
            );
            let time_str = hhmm(&cur.time);
            text_right(
                scene,
                tr,
                &format!("Local time {time_str}"),
                right - rb - 12.0,
                hy + 24.0,
                12.0,
                TextWeight::Regular,
                c.text_dim,
            );
        }

        let body_top = hy + 74.0;
        scene.fill(
            Fill::NonZero,
            t,
            c.border_standard.with_alpha_factor(0.6),
            None,
            &Rect::new(mx, body_top - 10.0, right, body_top - 9.0),
        );

        let Some(snap) = snap_opt else {
            self.max_scroll = 0.0;
            text_left(
                scene,
                tr,
                "Fetching current conditions…",
                mx,
                body_top + 24.0,
                15.0,
                TextWeight::Medium,
                c.text_dim,
            );
            return;
        };
        let units = self.units;
        let cur = &snap.current;

        // ---- Measure the scrollable body ----
        let gap = 12.0;
        let n_tiles = 5usize;
        let cols = balanced_tile_columns(mw, n_tiles);
        let tile_rows = n_tiles.div_ceil(cols);
        let tiles_block_h = tile_rows as f64 * TILE_H + (tile_rows as f64 - 1.0) * gap;
        let n_days = snap.daily.len().max(1);
        let days_block_h = 40.0 + n_days as f64 * DAY_ROW_H + 12.0;
        // Air-quality block: header + AQI hero + a row (or two) of pollutant
        // tiles. Reserved whether or not a reading is available yet, so the
        // scroll geometry stays stable while it loads.
        let aq = self.model.selected_air_quality().cloned();
        let aq_cols = if mw >= 560.0 { 4 } else { 2 };
        let aq_rows = 4usize.div_ceil(aq_cols);
        let aq_tiles_h = aq_rows as f64 * AQ_TILE_H + (aq_rows as f64 - 1.0) * gap;
        let aq_block_h = 40.0 + 64.0 + 12.0 + aq_tiles_h + 14.0;
        let body_h = HERO_H
            + SECTION_GAP
            + tiles_block_h
            + SECTION_GAP
            + HOURLY_CARD_H
            + SECTION_GAP
            + days_block_h
            + SECTION_GAP
            + aq_block_h;
        let view_h = (rect.y1 - PAD) - body_top;
        self.max_scroll = (body_h - view_h).max(0.0);
        self.scroll = self.scroll.clamp(0.0, self.max_scroll);

        let clip = Rect::new(rect.x0, body_top - 8.0, rect.x1, rect.y1);
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &clip);
        let mut y = body_top - self.scroll;

        // ---- Hero card ----
        let hero = Rect::new(mx, y, mx + mw, y + HERO_H);
        card(scene, t, hero, theme);
        let sky = Sky::from_code(cur.weather_code);
        icon::draw_icon(
            scene,
            t,
            sky,
            cur.is_day,
            Rect::new(
                hero.x0 + 24.0,
                hero.y0 + 36.0,
                hero.x0 + 116.0,
                hero.y0 + 128.0,
            ),
            theme,
        );
        let temp = snap.temp_in(cur.temperature, units).round() as i64;
        let temp_str = format!("{temp}");
        let temp_x = hero.x0 + 132.0;
        tr.draw_text_weighted(
            scene,
            &temp_str,
            72.0,
            c.text_primary,
            t * Affine::translate((temp_x, hero.y0 + 30.0)),
            None,
            TextWeight::Bold,
        );
        let (tnw, _) = tr.measure_weighted(&temp_str, 72.0, None, TextWeight::Bold);
        text_left(
            scene,
            tr,
            units.degree_glyph(),
            temp_x + f64::from(tnw) + 4.0,
            hero.y0 + 40.0,
            32.0,
            TextWeight::Regular,
            c.text_dim,
        );
        let cond = Sky::label(cur.weather_code);
        text_left(
            scene,
            tr,
            cond,
            temp_x + 2.0,
            hero.y0 + 114.0,
            19.0,
            TextWeight::Semibold,
            c.text_primary,
        );
        let feels = snap.temp_in(cur.apparent, units).round() as i64;
        text_left(
            scene,
            tr,
            &format!("Feels like {feels}{}", units.temp_suffix()),
            temp_x + 2.0,
            hero.y0 + 140.0,
            13.0,
            TextWeight::Regular,
            c.text_dim,
        );

        if let Some(today) = snap.daily.first() {
            let sr = hhmm(&today.sunrise);
            let ss = hhmm(&today.sunset);
            if mw >= 560.0 {
                self.draw_sun_arc(
                    scene,
                    t,
                    tr,
                    theme,
                    Rect::new(
                        hero.x1 - 226.0,
                        hero.y0 + 22.0,
                        hero.x1 - 22.0,
                        hero.y0 + HERO_H - 22.0,
                    ),
                    &cur.time,
                    &sr,
                    &ss,
                    cur.is_day,
                );
            } else {
                let sx = hero.x1 - 18.0;
                text_right(
                    scene,
                    tr,
                    "Sunrise",
                    sx,
                    hero.y0 + HERO_H - 60.0,
                    10.0,
                    TextWeight::Semibold,
                    c.text_dim,
                );
                text_right(
                    scene,
                    tr,
                    &sr,
                    sx,
                    hero.y0 + HERO_H - 46.0,
                    14.0,
                    TextWeight::Medium,
                    c.text_primary,
                );
                text_right(
                    scene,
                    tr,
                    "Sunset",
                    sx,
                    hero.y0 + HERO_H - 26.0,
                    10.0,
                    TextWeight::Semibold,
                    c.text_dim,
                );
                text_right(
                    scene,
                    tr,
                    &ss,
                    sx,
                    hero.y0 + HERO_H - 12.0,
                    14.0,
                    TextWeight::Medium,
                    c.text_primary,
                );
            }
        }

        y += HERO_H + SECTION_GAP;

        // ---- Metric tiles ----
        let wind = snap.wind_in(cur.wind_speed, units).round() as i64;
        let tiles: [(&str, String, String); 5] = [
            (
                "WIND",
                format!("{wind}"),
                format!(
                    "{} {}",
                    units::compass(cur.wind_direction),
                    units.wind_suffix()
                ),
            ),
            (
                "HUMIDITY",
                format!("{}", cur.humidity.round() as i64),
                "%".to_string(),
            ),
            (
                "PRECIP",
                format!("{:.1}", cur.precipitation),
                "mm now".to_string(),
            ),
            (
                "PRESSURE",
                format!("{}", cur.pressure.round() as i64),
                "hPa".to_string(),
            ),
            (
                "UV INDEX",
                format!("{}", cur.uv_index.round() as i64),
                uv_word(cur.uv_index).to_string(),
            ),
        ];
        // Full rows use even columns; a partial last row stretches its tiles to
        // fill the width, so there's never an orphaned tile with a dead gap.
        let full_rows = tiles.len() / cols;
        let rem = tiles.len() % cols;
        for (i, (label, value, sub)) in tiles.iter().enumerate() {
            let col = i % cols;
            let row = i / cols;
            let row_count = if row < full_rows { cols } else { rem };
            let tw = (mw - (row_count as f64 - 1.0) * gap) / row_count as f64;
            let tx = mx + col as f64 * (tw + gap);
            let ty = y + row as f64 * (TILE_H + gap);
            let tile = Rect::new(tx, ty, tx + tw, ty + TILE_H);
            card(scene, t, tile, theme);
            text_left(
                scene,
                tr,
                label,
                tile.x0 + 12.0,
                tile.y0 + 12.0,
                10.0,
                TextWeight::Semibold,
                c.text_dim,
            );
            text_left(
                scene,
                tr,
                value,
                tile.x0 + 12.0,
                tile.y0 + 30.0,
                25.0,
                TextWeight::Bold,
                c.text_primary,
            );
            text_left(
                scene,
                tr,
                sub,
                tile.x0 + 12.0,
                tile.y0 + TILE_H - 22.0,
                11.0,
                TextWeight::Regular,
                c.text_dim,
            );
        }

        y += tiles_block_h + SECTION_GAP;

        // ---- Hourly strip (24 fixed-width columns; scrolls horizontally) ----
        let hcard = Rect::new(mx, y, mx + mw, y + HOURLY_CARD_H);
        card(scene, t, hcard, theme);
        text_left(
            scene,
            tr,
            "Next 24 hours",
            hcard.x0 + 16.0,
            hcard.y0 + 12.0,
            14.0,
            TextWeight::Semibold,
            c.text_primary,
        );
        if self.hourly_max_scroll > 1.0 {
            text_right(
                scene,
                tr,
                "scroll →",
                hcard.x1 - 16.0,
                hcard.y0 + 14.0,
                11.0,
                TextWeight::Regular,
                c.text_dim,
            );
        }

        let cols: Vec<chart::HourColumn> = snap
            .hourly
            .iter()
            .enumerate()
            .map(|(i, h)| chart::HourColumn {
                hour: h.hour_label(),
                temp: snap.temp_in(h.temperature, units),
                precip: h.precip_probability,
                code: h.weather_code,
                is_day: h.is_day,
                is_now: i == 0,
            })
            .collect();

        // Each hour gets at least HOUR_COL_MIN_W; the strip scrolls when the 24
        // columns are wider than the band rather than squeezing them.
        let band = Rect::new(
            hcard.x0 + 14.0,
            hcard.y0 + 32.0,
            hcard.x1 - 14.0,
            hcard.y1 - 20.0,
        );
        let n = cols.len().max(1);
        let col_w = (band.width() / n as f64).max(HOUR_COL_MIN_W);
        let content_w = col_w * n as f64;
        self.hourly_viewport = band;
        self.hourly_content_w = content_w;
        self.hourly_max_scroll = (content_w - band.width()).max(0.0);
        self.hourly_scroll_x = self.hourly_scroll_x.clamp(0.0, self.hourly_max_scroll);

        chart::draw_hourly_strip(
            scene,
            t,
            band,
            &cols,
            col_w,
            self.hourly_scroll_x,
            accent,
            theme,
            tr,
            "°",
        );

        // Shared horizontal scrollbar when the strip overflows the band.
        let hmetrics = ScrollMetrics::new(content_w, band.width(), self.hourly_scroll_x);
        let track = Rect::new(band.x0, hcard.y1 - 12.0, band.x1, hcard.y1 - 6.0);
        self.hourly_track = track;
        if hmetrics.overflows()
            && let Some(thumb) = self.hourly_bar.thumb_rect(track, hmetrics)
        {
            scene.fill(
                Fill::NonZero,
                t,
                c.neutral[2].with_alpha_factor(0.5),
                None,
                &RoundedRect::from_rect(track, 3.0),
            );
            let thumb_color = if self.hourly_bar.is_active() {
                c.text_dim
            } else {
                c.text_dim.with_alpha_factor(0.7)
            };
            scene.fill(
                Fill::NonZero,
                t,
                thumb_color,
                None,
                &RoundedRect::from_rect(thumb, 3.0),
            );
        }

        y += HOURLY_CARD_H + SECTION_GAP;

        // ---- 7-day forecast ----
        let dcard = Rect::new(mx, y, mx + mw, y + days_block_h);
        card(scene, t, dcard, theme);
        text_left(
            scene,
            tr,
            "7-day forecast",
            dcard.x0 + 16.0,
            dcard.y0 + 12.0,
            14.0,
            TextWeight::Semibold,
            c.text_primary,
        );

        let (mut wmin, mut wmax) = (f64::INFINITY, f64::NEG_INFINITY);
        for d in &snap.daily {
            wmin = wmin.min(d.temp_min);
            wmax = wmax.max(d.temp_max);
        }
        if (wmax - wmin).abs() < 1.0 {
            wmax += 1.0;
            wmin -= 1.0;
        }

        let inner_l = dcard.x0 + 18.0;
        let inner_r = dcard.x1 - 16.0;
        let dw = inner_r - inner_l;
        let hi_w = 42.0;
        let bar_w = (dw * 0.22).clamp(56.0, 120.0);
        let bar_x1 = inner_r - hi_w;
        let bar_x0 = bar_x1 - bar_w;
        let lo_x = bar_x0 - 10.0;
        let precip_right = lo_x - 44.0;
        let precip_left = precip_right - 46.0;
        let show_precip = dw >= 300.0;
        let icon_l_x = inner_l + 92.0;
        let icon_r_x = icon_l_x + 28.0;
        let cond_x = icon_r_x + 10.0;
        // Per-day extras (UV-index max + wind max) get a fixed-width column to
        // the left of the precip/lo cluster, shown only when the row is wide
        // enough to fit them without crowding the condition text.
        let extras_w = 132.0;
        let extras_right = if show_precip {
            precip_left - 14.0
        } else {
            lo_x - 52.0
        };
        let extras_x = extras_right - extras_w;
        let show_extras = (extras_x - cond_x) >= 118.0;
        let cond_limit = if show_extras {
            extras_x - 12.0
        } else if show_precip {
            precip_left - 10.0
        } else {
            lo_x - 44.0
        };
        let show_cond = (cond_limit - cond_x) >= 96.0;

        let rows_top = dcard.y0 + 40.0;
        for (i, d) in snap.daily.iter().enumerate() {
            let ry = rows_top + i as f64 * DAY_ROW_H;
            let cy = ry + DAY_ROW_H / 2.0;
            if i > 0 {
                scene.fill(
                    Fill::NonZero,
                    t,
                    c.border_standard.with_alpha_factor(0.5),
                    None,
                    &Rect::new(inner_l, ry, inner_r, ry + 1.0),
                );
            }
            let day_label = match i {
                0 => "Today".to_string(),
                1 => "Tomorrow".to_string(),
                _ => weekday(&d.date),
            };
            text_left(
                scene,
                tr,
                &day_label,
                inner_l,
                cy - 8.0,
                14.0,
                TextWeight::Medium,
                c.text_primary,
            );
            let dsky = Sky::from_code(d.weather_code);
            icon::draw_icon(
                scene,
                t,
                dsky,
                true,
                Rect::new(icon_l_x, cy - 14.0, icon_r_x, cy + 14.0),
                theme,
            );
            if show_cond {
                text_left(
                    scene,
                    tr,
                    Sky::label(d.weather_code),
                    cond_x,
                    cy - 7.0,
                    13.0,
                    TextWeight::Regular,
                    c.text_dim,
                );
            }
            if show_extras {
                let (uv_label, wind_label) = day_extra_labels(d, &snap, units);
                // UV dot tinted by severity so the number reads at a glance.
                let uv_dot = if d.uv_index_max < 3.0 {
                    c.semantic.info
                } else if d.uv_index_max < 8.0 {
                    c.semantic.warning
                } else {
                    c.semantic.danger
                };
                scene.fill(
                    Fill::NonZero,
                    t,
                    uv_dot,
                    None,
                    &Circle::new(Point::new(extras_x + 4.0, cy), 3.0),
                );
                text_left(
                    scene,
                    tr,
                    &uv_label,
                    extras_x + 12.0,
                    cy - 7.0,
                    12.0,
                    TextWeight::Medium,
                    c.text_dim,
                );
                text_right(
                    scene,
                    tr,
                    &wind_label,
                    extras_right,
                    cy - 7.0,
                    12.0,
                    TextWeight::Regular,
                    c.text_dim,
                );
            }
            if show_precip {
                scene.fill(
                    Fill::NonZero,
                    t,
                    c.semantic.info,
                    None,
                    &Circle::new(Point::new(precip_left, cy), 3.0),
                );
                text_left(
                    scene,
                    tr,
                    &format!("{}%", d.precip_probability.round() as i64),
                    precip_left + 8.0,
                    cy - 7.0,
                    12.0,
                    TextWeight::Medium,
                    c.semantic.info,
                );
            }
            let lo = snap.temp_in(d.temp_min, units).round() as i64;
            let hi = snap.temp_in(d.temp_max, units).round() as i64;
            text_right(
                scene,
                tr,
                &format!("{lo}°"),
                lo_x,
                cy - 8.0,
                14.0,
                TextWeight::Medium,
                c.text_dim,
            );
            let track = RoundedRect::new(bar_x0, cy - 3.0, bar_x1, cy + 3.0, 3.0);
            scene.fill(Fill::NonZero, t, c.neutral[2], None, &track);
            let f0 = ((d.temp_min - wmin) / (wmax - wmin)).clamp(0.0, 1.0);
            let f1 = ((d.temp_max - wmin) / (wmax - wmin)).clamp(0.0, 1.0);
            let seg_x0 = bar_x0 + f0 * (bar_x1 - bar_x0);
            let seg_x1 = bar_x0 + f1 * (bar_x1 - bar_x0);
            let seg = RoundedRect::new(seg_x0, cy - 3.0, seg_x1.max(seg_x0 + 4.0), cy + 3.0, 3.0);
            let grad = Gradient::new_linear(Point::new(bar_x0, cy), Point::new(bar_x1, cy))
                .with_stops([(0.0_f32, c.semantic.info), (1.0_f32, c.semantic.warning)]);
            scene.fill(Fill::NonZero, t, &Brush::Gradient(grad), None, &seg);
            text_left(
                scene,
                tr,
                &format!("{hi}°"),
                bar_x1 + 8.0,
                cy - 8.0,
                14.0,
                TextWeight::Semibold,
                c.text_primary,
            );
        }

        y += days_block_h + SECTION_GAP;

        // ---- Air quality ----
        let acard = Rect::new(mx, y, mx + mw, y + aq_block_h);
        card(scene, t, acard, theme);
        text_left(
            scene,
            tr,
            "Air quality",
            acard.x0 + 16.0,
            acard.y0 + 12.0,
            14.0,
            TextWeight::Semibold,
            c.text_primary,
        );
        self.draw_air_quality(scene, t, tr, theme, acard, aq.as_ref(), aq_cols, gap);

        scene.pop_layer();

        // Status line + scroll indicator.
        if !self.status.is_empty() {
            text_left(
                scene,
                tr,
                &self.status,
                mx,
                rect.y1 - 20.0,
                11.0,
                TextWeight::Regular,
                c.text_dim,
            );
        }
        if self.max_scroll > 1.0 {
            let tx = rect.x1 - 6.0;
            let track_top = body_top + 2.0;
            let track_bot = rect.y1 - 6.0;
            let track_h = track_bot - track_top;
            let thumb_h = (track_h * (view_h / body_h)).clamp(28.0, track_h);
            let thumb_y = track_top + (self.scroll / self.max_scroll) * (track_h - thumb_h);
            scene.fill(
                Fill::NonZero,
                t,
                c.neutral[2].with_alpha_factor(0.6),
                None,
                &RoundedRect::new(tx, track_top, tx + 3.0, track_bot, 1.5),
            );
            scene.fill(
                Fill::NonZero,
                t,
                c.text_dim.with_alpha_factor(0.7),
                None,
                &RoundedRect::new(tx, thumb_y, tx + 3.0, thumb_y + thumb_h, 1.5),
            );
        }
    }

    fn draw_sky_background(&self, scene: &mut Scene, t: Affine, theme: &Theme, rect: Rect) {
        let c = &theme.tokens.colors;
        let base = c.surfaces.base;
        let top = if self.is_day() {
            lerp_color(base, c.accent[6], 0.20)
        } else {
            lerp_color(base, c.neutral[0], 0.55)
        };
        let grad = Gradient::new_linear(
            Point::new(rect.x0, rect.y0),
            Point::new(rect.x0, rect.y0 + rect.height() * 0.65),
        )
        .with_stops([(0.0_f32, top), (1.0_f32, base)]);
        scene.fill(Fill::NonZero, t, &Brush::Gradient(grad), None, &rect);
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_sun_arc(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        area: Rect,
        // Current observation time ("YYYY-MM-DDTHH:MM"); positions the sun dot.
        now_iso: &str,
        sunrise: &str,
        sunset: &str,
        is_day: bool,
    ) {
        let c = &theme.tokens.colors;
        let cx = f64::midpoint(area.x0, area.x1);
        let baseline = area.y1 - 24.0;
        let radius = (area.width() / 2.0 - 16.0).min(70.0);
        scene.fill(
            Fill::NonZero,
            t,
            c.border_standard,
            None,
            &Rect::new(area.x0 + 8.0, baseline, area.x1 - 8.0, baseline + 1.0),
        );
        let mut arc = BezPath::new();
        let steps = 28;
        for i in 0..=steps {
            let frac = f64::from(i) / f64::from(steps);
            let ang = std::f64::consts::PI * frac;
            let px = cx - radius * ang.cos();
            let py = baseline - radius * ang.sin();
            if i == 0 {
                arc.move_to(Point::new(px, py));
            } else {
                arc.line_to(Point::new(px, py));
            }
        }
        scene.stroke(
            &Stroke::new(1.5),
            t,
            c.text_dim.with_alpha_factor(0.7),
            None,
            &arc,
        );
        // Position the sun/moon dot from the real observation time relative to
        // today's sunrise/sunset (0 = sunrise at the left horizon, 0.5 = solar
        // noon at the apex, 1 = sunset at the right horizon). Falls back to a
        // neutral position only when the times can't be parsed.
        let now_hhmm = hhmm(now_iso);
        let frac =
            sun_arc_fraction(&now_hhmm, sunrise, sunset).unwrap_or(if is_day { 0.5 } else { 0.08 });
        let ang = std::f64::consts::PI * frac;
        let sx = cx - radius * ang.cos();
        let sy = baseline - radius * ang.sin();
        let sun_color = if is_day {
            c.semantic.warning
        } else {
            c.neutral[7]
        };
        scene.fill(
            Fill::NonZero,
            t,
            sun_color,
            None,
            &Circle::new(Point::new(sx, sy), 7.0),
        );
        text_left(
            scene,
            tr,
            "Sunrise",
            area.x0 + 4.0,
            baseline + 6.0,
            10.0,
            TextWeight::Semibold,
            c.text_dim,
        );
        text_left(
            scene,
            tr,
            sunrise,
            area.x0 + 4.0,
            baseline + 20.0,
            13.0,
            TextWeight::Medium,
            c.text_primary,
        );
        text_right(
            scene,
            tr,
            "Sunset",
            area.x1 - 4.0,
            baseline + 6.0,
            10.0,
            TextWeight::Semibold,
            c.text_dim,
        );
        text_right(
            scene,
            tr,
            sunset,
            area.x1 - 4.0,
            baseline + 20.0,
            13.0,
            TextWeight::Medium,
            c.text_primary,
        );
    }

    /// The air-quality section: an AQI severity hero + a row (or two) of key
    /// pollutant tiles, or an honest loading / unavailable message when there
    /// is no reading yet. Real data only — a missing value shows an em-dash,
    /// never a fabricated zero.
    #[allow(clippy::too_many_arguments)]
    fn draw_air_quality(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        area: Rect,
        aq: Option<&AirQuality>,
        cols: usize,
        gap: f64,
    ) {
        let c = &theme.tokens.colors;
        let ix = area.x0 + 16.0;
        let iw = area.width() - 32.0;
        let hero_top = area.y0 + 40.0;

        let Some(aq) = aq.filter(|a| a.has_any()) else {
            let msg = if self.is_fetching() {
                "Loading air quality…"
            } else {
                "Air quality unavailable"
            };
            text_center(
                scene,
                tr,
                msg,
                area.center().x,
                hero_top + 22.0,
                14.0,
                TextWeight::Medium,
                c.text_dim,
            );
            return;
        };

        // AQI hero: a severity-tinted chip with the big index number, the band
        // word, and a caption naming the scale.
        let hero = Rect::new(ix, hero_top, area.x1 - 16.0, hero_top + 64.0);
        if let Some(aqi) = aq.european_aqi {
            let sev = aqi_band_color(aqi, theme);
            let chip = RoundedRect::from_rect(
                Rect::new(hero.x0, hero.y0, hero.x0 + 116.0, hero.y1),
                f64::from(theme.tokens.radii.lg),
            );
            scene.fill(Fill::NonZero, t, sev.with_alpha_factor(0.16), None, &chip);
            scene.stroke(
                &Stroke::new(1.0),
                t,
                sev.with_alpha_factor(0.55),
                None,
                &chip,
            );
            let num = format!("{}", aqi.round() as i64);
            tr.draw_text_weighted(
                scene,
                &num,
                34.0,
                c.text_primary,
                t * Affine::translate((hero.x0 + 16.0, hero.y0 + 12.0)),
                None,
                TextWeight::Bold,
            );
            let (nw, _) = tr.measure_weighted(&num, 34.0, None, TextWeight::Bold);
            text_left(
                scene,
                tr,
                "AQI",
                hero.x0 + 24.0 + f64::from(nw),
                hero.y0 + 30.0,
                12.0,
                TextWeight::Semibold,
                c.text_dim,
            );
            text_left(
                scene,
                tr,
                api::european_aqi_band(aqi),
                hero.x0 + 132.0,
                hero.y0 + 12.0,
                19.0,
                TextWeight::Semibold,
                sev,
            );
            text_left(
                scene,
                tr,
                "European AQI · now",
                hero.x0 + 132.0,
                hero.y0 + 40.0,
                12.0,
                TextWeight::Regular,
                c.text_dim,
            );
        } else if let Some(us) = aq.us_aqi {
            let sev = aqi_band_color(us / 2.0, theme); // rough EU-equivalent tint
            text_left(
                scene,
                tr,
                &format!("{} US AQI", us.round() as i64),
                hero.x0,
                hero.y0 + 18.0,
                20.0,
                TextWeight::Bold,
                sev,
            );
        }

        // Key pollutant tiles (µg/m³).
        let tiles: [(&str, Option<f64>); 4] = [
            ("PM2.5", aq.pm2_5),
            ("PM10", aq.pm10),
            ("NO₂", aq.nitrogen_dioxide),
            ("O₃", aq.ozone),
        ];
        let tiles_top = hero.y1 + 12.0;
        let cols = cols.max(1);
        let tw = (iw - (cols as f64 - 1.0) * gap) / cols as f64;
        for (i, (label, val)) in tiles.iter().enumerate() {
            let col = i % cols;
            let row = i / cols;
            let tx = ix + col as f64 * (tw + gap);
            let ty = tiles_top + row as f64 * (AQ_TILE_H + gap);
            let tile = Rect::new(tx, ty, tx + tw, ty + AQ_TILE_H);
            card(scene, t, tile, theme);
            text_left(
                scene,
                tr,
                label,
                tile.x0 + 12.0,
                tile.y0 + 10.0,
                10.0,
                TextWeight::Semibold,
                c.text_dim,
            );
            match val {
                Some(v) => {
                    text_left(
                        scene,
                        tr,
                        &format!("{}", v.round() as i64),
                        tile.x0 + 12.0,
                        tile.y0 + 26.0,
                        22.0,
                        TextWeight::Bold,
                        c.text_primary,
                    );
                    text_left(
                        scene,
                        tr,
                        "µg/m³",
                        tile.x0 + 12.0,
                        tile.y1 - 18.0,
                        10.0,
                        TextWeight::Regular,
                        c.text_dim,
                    );
                }
                None => {
                    text_left(
                        scene,
                        tr,
                        "—",
                        tile.x0 + 12.0,
                        tile.y0 + 26.0,
                        20.0,
                        TextWeight::Regular,
                        c.text_dim,
                    );
                }
            }
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

    /// Handle an event routed to the detail pane. Returns true if consumed.
    pub fn handle_detail_event(&mut self, event: &WindowEvent, scale_factor: f64) -> bool {
        use winit::event::{ElementState, MouseButton, MouseScrollDelta};

        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_mouse = Point::new(position.x / scale_factor, position.y / scale_factor);
                // A live scrollbar drag tracks the cursor.
                if let Some(off) = self.hourly_bar.drag_to(
                    self.hourly_track,
                    self.hourly_metrics(),
                    self.last_mouse,
                ) {
                    self.hourly_scroll_x = off;
                    return true;
                }
                self.hourly_bar.update_hover(
                    self.hourly_track,
                    self.hourly_metrics(),
                    self.last_mouse,
                );
                false
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let (dx, dy) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => {
                        (f64::from(*x) * 48.0, f64::from(*y) * 48.0)
                    }
                    MouseScrollDelta::PixelDelta(p) => (p.x / scale_factor, p.y / scale_factor),
                };
                // Over the strip: scroll it horizontally. A horizontal delta
                // maps directly; a vertical wheel nudges it too, falling
                // through to the page scroll once it reaches an end.
                if self.hourly_viewport.contains(self.last_mouse) && self.hourly_max_scroll > 0.0 {
                    let want = if dx.abs() > f64::EPSILON { -dx } else { -dy };
                    let before = self.hourly_scroll_x;
                    self.hourly_scroll_x =
                        (self.hourly_scroll_x + want).clamp(0.0, self.hourly_max_scroll);
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
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                // The strip's scrollbar wins the press when it overflows.
                if self.hourly_max_scroll > 0.0 {
                    match self.hourly_bar.press(
                        self.hourly_track,
                        self.hourly_metrics(),
                        self.last_mouse,
                    ) {
                        PressOutcome::Thumb => return true,
                        PressOutcome::Page(off) => {
                            self.hourly_scroll_x = off;
                            return true;
                        }
                        PressOutcome::Miss => {}
                    }
                }
                if self.refresh_btn.contains(self.last_mouse) {
                    self.refresh_selected();
                    true
                } else {
                    false
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => self.hourly_bar.release(),
            _ => false,
        }
    }

    // ---- test / scenario seams --------------------------------------

    /// Stage a live mid-drag reorder of the second saved place for a headless
    /// shot: the chip floats free as a ghost under the cursor while the rest
    /// of the list has reflowed to open a gap near the tail where it will
    /// land. Insertion is finalised by `sync_insertion` against real geometry
    /// during the shot's draw pass.
    pub fn seed_drag(&mut self) {
        if self.model.locations.is_empty() {
            return;
        }
        let from = 1.min(self.model.locations.len() - 1);
        let id = self.model.locations[from].id;
        let start = Point::new(200.0, 232.0);
        let target = Point::new(200.0, start.y + 2.4 * (CHIP_H + CHIP_GAP));
        // Grab the chip at its vertical centre so the ghost sits on the cursor.
        self.reorder.begin(id, from, CHIP_H / 2.0, start);
        self.last_mouse = target;
        // A rough metrics is enough to clear the drag-start threshold; the draw
        // pass re-pins the insertion index against the real list geometry.
        let m = ListMetrics::uniform(0.0, CHIP_H, CHIP_GAP, self.model.locations.len());
        self.reorder.pointer_moved(target, &m);
        // Keep the classifier in the reorder state so the delete drop-zone and
        // list-bottom inset match a real in-flight drag.
        let mut press = PointerPress::new(from, start);
        press.update(target);
        self.press = Some(press);
    }

    /// Park the forecast scrolled to the bottom for a short-window shot.
    pub fn seed_scrolled(&mut self) {
        self.scroll = f64::MAX;
    }

    /// Stage the search field + suggestion overlay with a query and a set of
    /// geocoded matches (headless shot / test — the offscreen path can't await
    /// a live geocode). Drives the real shared [`Autocomplete`].
    pub fn seed_search(&mut self, query: &str, results: Vec<SavedLocation>) {
        self.search.begin_edit();
        self.search.set_text(query); // after begin_edit: caret at end, no select
        self.search_query = query.to_string();
        self.search_results = results;
        self.geocode_inflight = false;
        self.pending_query_at = None;
        self.sync_search_view();
    }

    /// Like [`seed_search`](Self::seed_search) but with the top match already
    /// highlighted — for a headless shot of the overlay mid-navigation.
    pub fn seed_search_highlighted(&mut self, query: &str, results: Vec<SavedLocation>) {
        self.seed_search(query, results);
        self.search.set_highlight(Some(0));
    }

    /// Stage the overlay's loading state (a geocode in flight).
    pub fn seed_search_loading(&mut self, query: &str) {
        self.search.begin_edit();
        self.search.set_text(query);
        self.search_query = query.to_string();
        self.search_results.clear();
        self.geocode_inflight = true;
        self.pending_query_at = None;
        self.sync_search_view();
    }

    /// Stage the overlay's "no matches" state.
    pub fn seed_search_empty(&mut self, query: &str) {
        self.search.begin_edit();
        self.search.set_text(query);
        self.search_query = query.to_string();
        self.search_results.clear();
        self.geocode_inflight = false;
        self.pending_query_at = None;
        self.sync_search_view();
    }

    #[cfg(test)]
    pub fn over_list_top(&self) -> f64 {
        self.over_list_top
    }
    #[cfg(test)]
    pub fn max_scroll(&self) -> f64 {
        self.max_scroll
    }
    #[cfg(test)]
    pub fn scroll(&self) -> f64 {
        self.scroll
    }
    #[cfg(test)]
    pub fn over_rect(&self) -> Rect {
        self.over_rect
    }
    #[cfg(test)]
    pub fn menu_is_open(&self) -> bool {
        self.menu.is_some()
    }
    /// The live hotspot rect the production hit-test would match for a given
    /// context-menu action, so tests click the *real* target rather than a
    /// recomputed guess. Requires a `draw_overview` pass with the menu open.
    #[cfg(test)]
    pub fn menu_item_rect(&self, action: EntryAction) -> Option<Rect> {
        self.over_hotspots
            .iter()
            .rev()
            .find_map(|h| match &h.action {
                OverAction::MenuItem(_, a) if *a == action => Some(h.rect),
                _ => None,
            })
    }
    #[cfg(test)]
    pub fn search_is_focused(&self) -> bool {
        self.search.is_editing()
    }
    #[cfg(test)]
    pub fn search_query(&self) -> &str {
        self.search.text()
    }
    /// Test seam: the number of suggestion rows the shared overlay currently
    /// offers (proves the autocomplete is populated from real geocode data).
    #[cfg(test)]
    pub fn search_suggestion_count(&self) -> usize {
        self.search.suggestions().len()
    }
    /// Test seam: the keyboard-highlighted suggestion's display label, if any.
    #[cfg(test)]
    pub fn search_highlight_label(&self) -> Option<String> {
        self.search.highlighted().map(|s| s.label.clone())
    }
    #[cfg(test)]
    pub fn hourly_max_scroll(&self) -> f64 {
        self.hourly_max_scroll
    }
    #[cfg(test)]
    pub fn hourly_scroll_x(&self) -> f64 {
        self.hourly_scroll_x
    }
    #[cfg(test)]
    pub fn hourly_viewport_rect(&self) -> Rect {
        self.hourly_viewport
    }
    #[cfg(test)]
    pub fn delete_zone_rect_test(&self) -> Rect {
        self.delete_zone_rect()
    }
    #[cfg(test)]
    pub fn refresh_btn_rect(&self) -> Rect {
        self.refresh_btn
    }
    /// Test seam: the live insertion boundary the reorder controller tracks
    /// this frame, or `None` when no reorder is in flight.
    #[cfg(test)]
    pub fn reorder_insertion_for_test(&self) -> Option<usize> {
        self.reorder.insertion()
    }
    /// Test seam: index of the chip currently being dragged, if any.
    #[cfg(test)]
    pub fn reorder_from_for_test(&self) -> Option<usize> {
        self.reorder.from_index()
    }
    /// Test seam: the reflow shift (logical px) the controller applies to the
    /// non-dragged chip at `i` while a drag is live.
    #[cfg(test)]
    pub fn reorder_item_shift_for_test(&self, i: usize) -> f64 {
        self.reorder.item_shift(i, &self.list_metrics())
    }
}

// =====================================================================
// Free drawing helpers
// =====================================================================

fn tile_columns(mw: f64) -> usize {
    let gap = 12.0;
    (((mw + gap) / (TILE_MIN_W + gap)).floor() as usize).clamp(2, 5)
}

/// Column count for `n` metric tiles at the given content width, adjusted to
/// avoid a single orphaned tile on the last row. For the five weather tiles a
/// raw fit of four columns (4 + 1) is dropped to three (a balanced 3 + 2).
fn balanced_tile_columns(mw: f64, n: usize) -> usize {
    let cols = tile_columns(mw);
    if cols > 2 && n % cols == 1 {
        cols - 1
    } else {
        cols
    }
}

fn card(scene: &mut Scene, t: Affine, rect: Rect, theme: &Theme) {
    let c = &theme.tokens.colors;
    let shape = RoundedRect::from_rect(rect, f64::from(theme.tokens.radii.lg));
    scene.fill(Fill::NonZero, t, c.surfaces.raised, None, &shape);
    scene.stroke(
        &Stroke::new(1.0),
        t,
        c.border_standard.with_alpha_factor(0.7),
        None,
        &shape,
    );
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
    tr.draw_text_weighted(
        scene,
        s,
        size,
        color,
        Affine::translate((x, y_top)),
        None,
        weight,
    );
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
        Affine::translate((x_right - f64::from(w), y_top)),
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
        Affine::translate((x_center - f64::from(w) / 2.0, y_top)),
        None,
        weight,
    );
}

fn lerp_color(a: Color, b: Color, t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    let a = a.to_rgba8();
    let b = b.to_rgba8();
    let mix = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * t) as u8;
    Color::from_rgba8(mix(a.r, b.r), mix(a.g, b.g), mix(a.b, b.b), mix(a.a, b.a))
}

fn hhmm(iso: &str) -> String {
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
fn sun_arc_fraction(now_hhmm: &str, sunrise_hhmm: &str, sunset_hhmm: &str) -> Option<f64> {
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
        .map_or_else(|_| date.to_string(), |d| d.format("%a").to_string())
}

fn fmt_date(iso: &str) -> String {
    use chrono::NaiveDate;
    let date_part = iso.get(0..10).unwrap_or(iso);
    NaiveDate::parse_from_str(date_part, "%Y-%m-%d").map_or_else(
        |_| date_part.to_string(),
        |d| d.format("%a, %b %d").to_string(),
    )
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

/// Severity tint for a European AQI value, drawn from the theme's semantic
/// palette so it tracks light/dark and high-contrast.
fn aqi_band_color(aqi: f64, theme: &Theme) -> Color {
    let c = &theme.tokens.colors;
    match aqi.round() as i64 {
        i64::MIN..=20 => c.semantic.success,
        21..=40 => c.semantic.info,
        41..=60 => c.semantic.warning,
        _ => c.semantic.danger,
    }
}

/// The encoded, unambiguous value for a geocoded search result — its display
/// name plus subtitle — so the accepted [`Autocomplete`] value maps back to the
/// exact place (two cities named "Paris" stay distinct).
fn result_value(loc: &SavedLocation) -> String {
    format!("{}, {}", loc.name, loc.subtitle())
}

#[cfg(test)]
mod layout_tests {
    use super::balanced_tile_columns;

    #[test]
    fn five_tiles_never_orphan_a_single_on_the_last_row() {
        // Across representative content widths the column count never leaves a
        // lonely single tile (rem == 1) when more than two columns fit.
        for mw in [404.0_f64, 504.0, 604.0, 884.0, 1200.0] {
            let cols = balanced_tile_columns(mw, 5);
            if cols > 2 {
                assert_ne!(5 % cols, 1, "mw {mw}: {cols} columns orphans one tile");
            }
        }
        // The specific awkward case: a raw 4-wide fit becomes a balanced 3 + 2.
        assert_eq!(balanced_tile_columns(604.0, 5), 3);
    }
}

#[cfg(test)]
mod helper_tests {
    use super::{day_extra_labels, hhmm_to_minutes, sun_arc_fraction};
    use crate::units::Units;

    #[test]
    fn sun_arc_fraction_maps_time_to_arc_position() {
        // The dot the hero renders is driven by this: sunrise → 0 (left
        // horizon), solar noon → 0.5 (apex), sunset → 1 (right horizon).
        assert_eq!(sun_arc_fraction("06:00", "06:00", "18:00"), Some(0.0));
        assert_eq!(sun_arc_fraction("12:00", "06:00", "18:00"), Some(0.5));
        assert_eq!(sun_arc_fraction("18:00", "06:00", "18:00"), Some(1.0));
        assert_eq!(sun_arc_fraction("09:00", "06:00", "18:00"), Some(0.25));
    }

    #[test]
    fn sun_arc_fraction_clamps_outside_daylight() {
        // Before sunrise pins to the left horizon, after sunset to the right.
        assert_eq!(sun_arc_fraction("03:00", "06:00", "18:00"), Some(0.0));
        assert_eq!(sun_arc_fraction("23:30", "06:00", "18:00"), Some(1.0));
    }

    #[test]
    fn sun_arc_fraction_rejects_unusable_input() {
        assert_eq!(sun_arc_fraction("--:--", "06:00", "18:00"), None);
        assert_eq!(
            sun_arc_fraction("12:00", "18:00", "06:00"),
            None,
            "sunset before sunrise"
        );
        assert_eq!(
            sun_arc_fraction("12:00", "12:00", "12:00"),
            None,
            "zero-length day"
        );
        assert_eq!(hhmm_to_minutes("25:00"), None);
        assert_eq!(hhmm_to_minutes("07:30"), Some(450));
    }

    #[test]
    fn day_extra_labels_render_real_uv_and_wind_in_display_units() {
        // The values the 7-day rows now surface come straight from the parsed
        // per-day fields (previously stored but never rendered).
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
