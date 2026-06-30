//! Root widget for the Maps app.
//!
//! `MapsApp` owns the in-memory [`MapModel`] (mirrored into the CRDT every
//! frame via [`crate::doc::MapsDoc`]) and the static vector [`Basemap`]. It
//! is a single full-window custom widget: the map is full-bleed and the UI
//! (search bar, side panel, zoom controls, place card) floats on top, drawn
//! straight into the Vello scene with theme-derived colours. Interaction —
//! pan, zoom-to-cursor, marker hit-testing, panel buttons, search typing —
//! is handled directly in `handle_event`.

use std::collections::HashMap;
use std::sync::mpsc::{Receiver, Sender};
use std::time::Instant;

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{BlendMode, Color, Fill};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::presence::PresenceCtx;
use ui::permissions::PermissionContext;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::Widget;

use crate::canvas::{self, MapPalette};
use crate::doc::MapsDoc;
use crate::draw;
use crate::geo::{self, Projection};
use crate::model::{Category, MapModel, Panel, TransitMode, TravelMode};
use crate::net::{self, NetMsg};
use crate::sample::{self, Basemap};
use crate::tiles::{GeoBounds, TileLayer};

// --- Layout metrics (logical px) -------------------------------------------
const MARGIN: f64 = 16.0;
const PANEL_W: f64 = 360.0;
const SEARCH_H: f64 = 48.0;
const GAP: f64 = 10.0;
const TAB_H: f64 = 34.0;
const CTRL_SIZE: f64 = 40.0;
const ROW_H: f64 = 56.0;

/// A clickable hot-region recorded during draw and hit-tested on click.
#[derive(Clone, Debug)]
enum Hit {
    /// Swallow the click (panel chrome) — don't pan the map.
    Consume,
    FocusSearch,
    ClearSearch,
    Tab(Panel),
    Mode(TravelMode),
    SelectPlace(String),
    Recent(String),
    ZoomIn,
    ZoomOut,
    Locate,
    Directions,
    Transit,
    ToggleSave,
    ClearRoute,
    SwapEnds,
    Journey(usize),
    /// Open the offline-download / manage-cache overlay.
    OfflineOpen,
    /// Close the overlay.
    OfflineClose,
    /// Adjust how many extra zoom levels the offline region covers (±1).
    OfflineZoomDelta(i8),
    /// Begin downloading the current viewport region for offline use.
    OfflineDownload,
    /// Delete the on-disk tile cache.
    OfflineClearCache,
}

pub struct MapsApp {
    model: MapModel,
    basemap: Basemap,
    rect: Rect,
    node: Option<NodeId>,

    // interaction state (render/UI caches only — app state lives in `model`)
    last_mouse: Point,
    search_focused: bool,
    left_down: bool,
    dragging: bool,
    drag_last: Point,
    drag_moved: bool,
    click_targets: Vec<(Rect, Hit)>,
    scroll: f64,
    content_h: f64,
    content_view_h: f64,
    start: Instant,

    // CRDT bridge
    loaded: bool,
    dirty: bool,

    // networking
    net_tx: Sender<NetMsg>,
    net_rx: Receiver<NetMsg>,
    pending: u32,

    // raster basemap (non-blocking OSM tiles)
    tiles: TileLayer,

    // offline-download / cache-management overlay
    offline_open: bool,
    offline_zoom_extra: u8,
    cache_bytes: u64,
}

impl Default for MapsApp {
    fn default() -> Self {
        Self::new()
    }
}

impl MapsApp {
    /// Live app: spawns the background tile worker (network + decode off the
    /// UI thread).
    pub fn new() -> Self {
        Self::with_tiles(TileLayer::new())
    }

    /// Headless/offline variant used by `--shots` and tests: no worker thread,
    /// no network. The stylised vector basemap stands in for raster imagery so
    /// the screenshot is fully populated without ever touching the wire.
    pub fn offline() -> Self {
        Self::with_tiles(TileLayer::offline())
    }

    fn with_tiles(tiles: TileLayer) -> Self {
        let (net_tx, net_rx) = std::sync::mpsc::channel();
        let cache_bytes = tiles.cache_bytes();
        Self {
            model: sample::default_model(),
            basemap: sample::basemap(),
            rect: Rect::ZERO,
            node: None,
            last_mouse: Point::ZERO,
            search_focused: false,
            left_down: false,
            dragging: false,
            drag_last: Point::ZERO,
            drag_moved: false,
            click_targets: Vec::new(),
            scroll: 0.0,
            content_h: 0.0,
            content_view_h: 0.0,
            start: Instant::now(),
            loaded: false,
            dirty: true,
            net_tx,
            net_rx,
            pending: 0,
            tiles,
            offline_open: false,
            offline_zoom_extra: 2,
            cache_bytes,
        }
    }

    // --- geometry helpers --------------------------------------------------
    fn view_center(&self) -> Point {
        Point::new(
            (self.rect.x0 + self.rect.x1) / 2.0,
            (self.rect.y0 + self.rect.y1) / 2.0,
        )
    }

    fn projection(&self) -> Projection {
        Projection::new(self.model.center, self.model.zoom, self.view_center())
    }

    fn search_rect(&self) -> Rect {
        let x0 = self.rect.x0 + MARGIN;
        let y0 = self.rect.y0 + MARGIN;
        Rect::new(x0, y0, x0 + PANEL_W, y0 + SEARCH_H)
    }

    fn panel_rect(&self) -> Rect {
        let x0 = self.rect.x0 + MARGIN;
        let top = self.search_rect().y1 + GAP;
        Rect::new(x0, top, x0 + PANEL_W, self.rect.y1 - MARGIN)
    }

    fn zoom_in_rect(&self) -> Rect {
        let x1 = self.rect.x1 - MARGIN;
        let y0 = self.rect.y0 + MARGIN;
        Rect::new(x1 - CTRL_SIZE, y0, x1, y0 + CTRL_SIZE)
    }
    fn zoom_out_rect(&self) -> Rect {
        let r = self.zoom_in_rect();
        Rect::new(r.x0, r.y1, r.x1, r.y1 + CTRL_SIZE)
    }
    fn locate_rect(&self) -> Rect {
        let r = self.zoom_out_rect();
        Rect::new(r.x0, r.y1 + GAP, r.x1, r.y1 + GAP + CTRL_SIZE)
    }
    fn download_rect(&self) -> Rect {
        let r = self.locate_rect();
        Rect::new(r.x0, r.y1 + GAP, r.x1, r.y1 + GAP + CTRL_SIZE)
    }

    /// The geographic bounds currently visible in the map viewport.
    fn viewport_bounds(&self) -> GeoBounds {
        let proj = self.projection();
        let nw = proj.unproject(Point::new(self.rect.x0, self.rect.y0));
        let se = proj.unproject(Point::new(self.rect.x1, self.rect.y1));
        GeoBounds::new(nw, se)
    }

    // --- actions -----------------------------------------------------------
    fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    fn select_place(&mut self, id: &str) {
        self.model.selected = Some(id.to_string());
        if let Some(p) = self.model.place(id) {
            self.model.center = p.pos;
        }
        self.mark_dirty();
    }

    fn set_panel(&mut self, panel: Panel) {
        self.model.panel = panel;
        self.scroll = 0.0;
        if panel == Panel::Directions && self.model.route.is_none() {
            self.recompute_route();
        }
        self.mark_dirty();
    }

    /// Local (offline) search: substring match over known places. Also kicks
    /// off a live Nominatim request whose results fold in later.
    fn run_search(&mut self) {
        let q = self.model.query.trim().to_string();
        if q.is_empty() {
            return;
        }
        let needle = q.to_lowercase();
        let mut results: Vec<_> = self
            .model
            .places
            .iter()
            .filter(|p| {
                p.name.to_lowercase().contains(&needle)
                    || p.address.to_lowercase().contains(&needle)
                    || p.category.label().to_lowercase().contains(&needle)
            })
            .cloned()
            .collect();
        if results.is_empty() {
            // Fall back to category keyword matches so a term like "coffee"
            // still surfaces cafés.
            results = self
                .model
                .places
                .iter()
                .filter(|p| keyword_match(&needle, p.category))
                .cloned()
                .collect();
        }
        if let Some(first) = results.first() {
            let id = first.id.clone();
            self.model.center = first.pos;
            self.model.selected = Some(id);
        }
        self.model.results = results;
        self.model.push_recent(&q);
        self.model.panel = Panel::Explore;
        self.scroll = 0.0;

        // Live augmentation (best effort).
        self.spawn_search(q);
        self.mark_dirty();
    }

    fn spawn_search(&mut self, query: String) {
        let tx = self.net_tx.clone();
        self.pending += 1;
        self.model.status = "Searching…".to_string();
        std::thread::spawn(move || {
            let results = net::geocode(&query);
            let _ = tx.send(NetMsg::Search { query, results });
        });
    }

    fn recompute_route(&mut self) {
        let dest = match self.model.selected_place() {
            Some(p) => p.pos,
            None => return,
        };
        let origin = self.model.origin.unwrap_or_else(sample::origin);
        self.model.origin = Some(origin);
        let mode = self.model.mode;
        // Immediate offline fallback so the panel is never empty.
        self.model.route = Some(sample::fallback_route(origin, dest, mode));
        self.fit_route();

        // Live OSRM request.
        let tx = self.net_tx.clone();
        self.pending += 1;
        self.model.status = "Routing…".to_string();
        std::thread::spawn(move || {
            let r = net::route(origin, dest, mode);
            let _ = tx.send(NetMsg::Route(r));
        });
        self.mark_dirty();
    }

    fn recompute_transit(&mut self) {
        let dest = match self.model.selected_place() {
            Some(p) => p.pos,
            None => return,
        };
        let origin = self.model.origin.unwrap_or_else(sample::origin);
        self.model.origin = Some(origin);
        if self.model.transit.is_empty() {
            self.model.transit = sample::sample_transit();
        }
        self.model.panel = Panel::Transit;
        self.model.transit_choice = 0;
        let tx = self.net_tx.clone();
        self.pending += 1;
        self.model.status = "Finding departures…".to_string();
        std::thread::spawn(move || {
            let r = net::transit(origin, dest);
            let _ = tx.send(NetMsg::Transit(r));
        });
        self.mark_dirty();
    }

    fn set_mode(&mut self, mode: TravelMode) {
        if self.model.mode != mode {
            self.model.mode = mode;
            self.recompute_route();
        }
    }

    fn toggle_save_selected(&mut self) {
        if let Some(id) = self.model.selected.clone() {
            self.model.toggle_saved(&id);
            self.mark_dirty();
        }
    }

    fn clear_route(&mut self) {
        self.model.route = None;
        self.mark_dirty();
    }

    fn swap_ends(&mut self) {
        if let (Some(o), Some(sel)) = (self.model.origin, self.model.selected_place().map(|p| p.pos)) {
            self.model.origin = Some(sel);
            // Selected place stays the destination conceptually; we just move
            // origin to the old destination and re-route from the old origin.
            let _ = o;
            self.recompute_route();
        }
    }

    fn fit_route(&mut self) {
        if let Some(route) = &self.model.route {
            let w = self.rect.width() - (PANEL_W + MARGIN * 2.0 + GAP);
            let h = self.rect.height();
            if let Some((c, z)) = geo::fit_bounds(&route.geometry, w.max(200.0), h, 70.0) {
                self.model.center = c;
                self.model.zoom = z.min(16.5);
            }
        }
    }

    fn zoom_by(&mut self, dz: f64, anchor: Point) {
        let proj = self.projection();
        let under = proj.unproject(anchor);
        self.model.zoom = (self.model.zoom + dz).clamp(3.0, 19.0);
        let proj2 = Projection::new(self.model.center, self.model.zoom, self.view_center());
        let q = proj2.project(under);
        let cw = geo::lonlat_to_world(self.model.center, self.model.zoom);
        self.model.center = geo::world_to_lonlat(
            cw.0 + (q.x - anchor.x),
            cw.1 + (q.y - anchor.y),
            self.model.zoom,
        );
        self.mark_dirty();
    }

    fn pan_by(&mut self, dx: f64, dy: f64) {
        let cw = geo::lonlat_to_world(self.model.center, self.model.zoom);
        self.model.center = geo::world_to_lonlat(cw.0 - dx, cw.1 - dy, self.model.zoom);
        self.mark_dirty();
    }

    /// Hit-test a click against the markers; returns the place id if any.
    fn marker_at(&self, p: Point) -> Option<String> {
        let proj = self.projection();
        let mut best: Option<(f64, String)> = None;
        for place in &self.model.places {
            let anchor = proj.project(place.pos);
            let selected = self.model.selected.as_deref() == Some(place.id.as_str());
            let h = if selected { 30.0 } else { 22.0 };
            let r = if selected { 13.0 } else { 9.0 };
            let center = Point::new(anchor.x, anchor.y - h);
            let d = center.distance(p);
            if d <= r + 5.0 {
                if best.as_ref().map(|(bd, _)| d < *bd).unwrap_or(true) {
                    best = Some((d, place.id.clone()));
                }
            }
        }
        best.map(|(_, id)| id)
    }

    fn drain_net(&mut self) {
        while let Ok(msg) = self.net_rx.try_recv() {
            self.pending = self.pending.saturating_sub(1);
            match msg {
                NetMsg::Search { query, results } => match results {
                    Ok(found) if !found.is_empty() => {
                        for p in &found {
                            self.model.upsert_place(p.clone());
                        }
                        self.model.results = found;
                        if let Some(first) = self.model.results.first() {
                            self.model.center = first.pos;
                            self.model.selected = Some(first.id.clone());
                        }
                        self.model.status = format!("{} results for “{}”", self.model.results.len(), query);
                    }
                    Ok(_) => self.model.status = format!("No results for “{query}”"),
                    Err(e) => self.model.status = format!("Search failed: {e}"),
                },
                NetMsg::Route(r) => match r {
                    Ok(route) => {
                        self.model.route = Some(route);
                        self.fit_route();
                        self.model.status = "Route updated".to_string();
                    }
                    Err(e) => self.model.status = format!("Routing failed (offline estimate shown): {e}"),
                },
                NetMsg::Transit(t) => match t {
                    Ok(j) if !j.is_empty() => {
                        self.model.transit = j;
                        self.model.status = "Departures updated".to_string();
                    }
                    Ok(_) => self.model.status = "No transit options found".to_string(),
                    Err(e) => self.model.status = format!("Transit lookup failed: {e}"),
                },
            }
            if self.pending == 0 && self.model.status.contains('…') {
                self.model.status.clear();
            }
            self.mark_dirty();
        }
    }

    // --- click dispatch ----------------------------------------------------
    fn dispatch(&mut self, hit: &Hit) {
        match hit {
            Hit::Consume => {}
            Hit::FocusSearch => self.search_focused = true,
            Hit::ClearSearch => {
                self.model.query.clear();
                self.model.results.clear();
                self.mark_dirty();
            }
            Hit::Tab(p) => self.set_panel(*p),
            Hit::Mode(m) => self.set_mode(*m),
            Hit::SelectPlace(id) => {
                self.select_place(id);
            }
            Hit::Recent(q) => {
                self.model.query = q.clone();
                self.run_search();
            }
            Hit::ZoomIn => self.zoom_by(1.0, self.view_center()),
            Hit::ZoomOut => self.zoom_by(-1.0, self.view_center()),
            Hit::Locate => {
                if let Some(o) = self.model.origin.or(Some(sample::origin())) {
                    self.model.origin = Some(o);
                    self.model.center = o;
                    self.mark_dirty();
                }
            }
            Hit::Directions => {
                self.model.panel = Panel::Directions;
                self.recompute_route();
            }
            Hit::Transit => self.recompute_transit(),
            Hit::ToggleSave => self.toggle_save_selected(),
            Hit::ClearRoute => self.clear_route(),
            Hit::SwapEnds => self.swap_ends(),
            Hit::Journey(i) => {
                self.model.transit_choice = *i;
                self.mark_dirty();
            }
            Hit::OfflineOpen => {
                self.offline_open = true;
                self.cache_bytes = self.tiles.cache_bytes();
            }
            Hit::OfflineClose => self.offline_open = false,
            Hit::OfflineZoomDelta(d) => {
                let v = self.offline_zoom_extra as i16 + *d as i16;
                self.offline_zoom_extra = v.clamp(0, 6) as u8;
            }
            Hit::OfflineDownload => {
                let base_z = TileLayer::tile_zoom(self.model.zoom);
                let z1 = (base_z + self.offline_zoom_extra).min(19);
                let bounds = self.viewport_bounds();
                let n = self.tiles.start_download(bounds, base_z, z1);
                self.model.status = format!("Downloading {n} tiles for offline use…");
            }
            Hit::OfflineClearCache => {
                let freed = self.tiles.clear_cache();
                self.cache_bytes = 0;
                self.model.status = format!("Cleared {} of offline tiles", fmt_bytes(freed));
            }
        }
    }
}

/// Loose keyword → category synonyms for offline search.
fn keyword_match(needle: &str, cat: Category) -> bool {
    let syn: &[&str] = match cat {
        Category::Cafe => &["coffee", "cafe", "café", "espresso", "breakfast"],
        Category::Restaurant => &["food", "eat", "lunch", "dinner", "restaurant"],
        Category::Bar => &["beer", "drink", "pub", "bar", "biergarten"],
        Category::Museum => &["art", "museum", "gallery", "history"],
        Category::Park => &["park", "green", "garden", "nature"],
        Category::Transit => &["station", "train", "metro", "transit", "bahnhof"],
        Category::Hotel => &["hotel", "stay", "sleep", "room"],
        Category::Shop => &["shop", "market", "store", "buy"],
        Category::Landmark => &["sight", "landmark", "tourist"],
        _ => &[],
    };
    syn.iter().any(|s| s.contains(needle) || needle.contains(*s))
}

fn vmid(y: f64, h: f64, size: f32) -> f64 {
    y + (h - size as f64 * 1.25) / 2.0
}

/// Human-readable byte count for the cache-size readouts.
fn fmt_bytes(n: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    if n >= GB {
        format!("{:.1} GB", n as f64 / GB as f64)
    } else if n >= MB {
        format!("{:.1} MB", n as f64 / MB as f64)
    } else if n >= KB {
        format!("{} KB", n / KB)
    } else {
        format!("{n} B")
    }
}

fn transit_color(theme: &Theme, mode: TransitMode) -> Color {
    let c = &theme.tokens.colors;
    match mode {
        TransitMode::Walk => c.text_dim,
        TransitMode::Bus => c.semantic.info,
        TransitMode::Tram => c.semantic.danger,
        TransitMode::Subway => theme.accent_primary,
        TransitMode::Rail => c.semantic.success,
        TransitMode::Ferry => c.accent[6],
    }
}

// ===========================================================================
// Drawing
// ===========================================================================
impl MapsApp {
    fn draw_app(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        self.click_targets.clear();
        let pal = MapPalette::from_theme(theme);
        let proj = self.projection();
        let clip = self.rect;

        // Map layers. Real OSM raster tiles paint first (non-blocking); the
        // stylised vector basemap is the fallback shown only until imagery is
        // available (headless `--shots`, or the very first offline frame).
        let view_center = self.view_center();
        let painted_tiles =
            canvas::draw_tiles(scene, t, clip, &mut self.tiles, &self.model, view_center, &pal);
        if !painted_tiles {
            canvas::draw_basemap(scene, t, clip, &proj, &self.basemap, &pal);
        }
        if let Some(route) = &self.model.route {
            canvas::draw_route(scene, t, &proj, route, &pal);
        }
        canvas::draw_markers(scene, t, tr, &proj, &self.model, theme, &pal, clip);
        canvas::draw_scale_and_attribution(scene, t, tr, clip, &self.model, theme);

        // Floating UI.
        self.draw_controls(scene, t, tr, theme);
        self.draw_panel(scene, t, tr, theme);
        self.draw_search(scene, t, tr, theme);
        self.draw_status(scene, t, tr, theme);
        if self.offline_open {
            self.draw_offline_overlay(scene, t, tr, theme);
        }
    }

    fn draw_search(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let r = self.search_rect();
        let c = &theme.tokens.colors;
        let border = if self.search_focused { theme.border_focus } else { c.border_standard };
        draw::card(scene, t, r, 12.0, c.surfaces.raised, border);
        // Search glyph (a magnifier drawn as a circle + handle).
        let gx = r.x0 + 22.0;
        let gy = r.y0 + r.height() / 2.0;
        draw::stroke_circle(scene, t, Point::new(gx, gy - 1.0), 6.0, 2.0, theme.text_dim);
        draw::line(
            scene,
            t,
            Point::new(gx + 4.5, gy + 3.5),
            Point::new(gx + 9.0, gy + 8.0),
            2.0,
            theme.text_dim,
        );

        let text_x = r.x0 + 40.0;
        let avail = r.x1 - text_x - 34.0;
        let size = 15.0_f32;
        let ty = vmid(r.y0, r.height(), size);
        if self.model.query.is_empty() {
            draw::text(scene, tr, t, "Search places, addresses…", text_x, ty, size, theme.text_dim);
        } else {
            draw::text_clip(scene, tr, t, &self.model.query, text_x, ty, size, theme.text_primary, avail);
            // Caret when focused.
            if self.search_focused && self.caret_on() {
                let w = draw::measure_w(tr, &self.model.query, size).min(avail);
                draw::line(
                    scene,
                    t,
                    Point::new(text_x + w + 2.0, r.y0 + 12.0),
                    Point::new(text_x + w + 2.0, r.y1 - 12.0),
                    1.5,
                    theme.accent_primary,
                );
            }
            // Clear (×) button.
            let cb = Rect::new(r.x1 - 32.0, r.y0 + 10.0, r.x1 - 10.0, r.y1 - 10.0);
            draw::fill_circle(scene, t, cb.center(), 10.0, c.surfaces.base);
            let cc = cb.center();
            draw::line(scene, t, Point::new(cc.x - 4.0, cc.y - 4.0), Point::new(cc.x + 4.0, cc.y + 4.0), 1.8, theme.text_dim);
            draw::line(scene, t, Point::new(cc.x + 4.0, cc.y - 4.0), Point::new(cc.x - 4.0, cc.y + 4.0), 1.8, theme.text_dim);
            self.click_targets.push((cb, Hit::ClearSearch));
        }
        // Whole bar focuses search (added before the clear button so the
        // reverse hit-test lets the clear button win).
        self.click_targets.insert(0, (r, Hit::FocusSearch));
    }

    fn caret_on(&self) -> bool {
        (self.start.elapsed().as_secs_f64() * 1.6) as u64 % 2 == 0
    }

    fn draw_controls(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let c = &theme.tokens.colors;
        // Zoom in/out as one stacked card.
        let zi = self.zoom_in_rect();
        let zo = self.zoom_out_rect();
        let combined = Rect::new(zi.x0, zi.y0, zi.x1, zo.y1);
        draw::card(scene, t, combined, 10.0, c.surfaces.raised, c.border_standard);
        draw::line(scene, t, Point::new(zi.x0 + 8.0, zi.y1), Point::new(zi.x1 - 8.0, zi.y1), 1.0, c.border_standard);
        draw::text_center(scene, tr, t, "+", zi.x0, zi.x1, vmid(zi.y0, zi.height(), 22.0), 22.0, theme.text_primary);
        draw::text_center(scene, tr, t, "–", zo.x0, zo.x1, vmid(zo.y0, zo.height(), 22.0), 22.0, theme.text_primary);
        self.click_targets.push((zi, Hit::ZoomIn));
        self.click_targets.push((zo, Hit::ZoomOut));

        // Locate button.
        let lc = self.locate_rect();
        draw::card(scene, t, lc, 10.0, c.surfaces.raised, c.border_standard);
        let ctr = lc.center();
        draw::stroke_circle(scene, t, ctr, 6.0, 2.0, theme.accent_primary);
        draw::fill_circle(scene, t, ctr, 2.0, theme.accent_primary);
        for (dx, dy) in [(0.0, -9.0), (0.0, 9.0), (-9.0, 0.0), (9.0, 0.0)] {
            draw::line(scene, t, Point::new(ctr.x + dx * 0.7, ctr.y + dy * 0.7), Point::new(ctr.x + dx, ctr.y + dy), 2.0, theme.accent_primary);
        }
        self.click_targets.push((lc, Hit::Locate));

        // Offline-download button: a downward arrow into a tray.
        let dl = self.download_rect();
        let dl_active = self.tiles.progress().is_active();
        let dl_border = if dl_active { theme.accent_primary } else { c.border_standard };
        draw::card(scene, t, dl, 10.0, c.surfaces.raised, dl_border);
        let dc = dl.center();
        draw::line(scene, t, Point::new(dc.x, dc.y - 8.0), Point::new(dc.x, dc.y + 3.0), 2.0, theme.text_primary);
        draw::line(scene, t, Point::new(dc.x - 4.0, dc.y - 1.0), Point::new(dc.x, dc.y + 3.0), 2.0, theme.text_primary);
        draw::line(scene, t, Point::new(dc.x + 4.0, dc.y - 1.0), Point::new(dc.x, dc.y + 3.0), 2.0, theme.text_primary);
        draw::line(scene, t, Point::new(dc.x - 6.0, dc.y + 7.0), Point::new(dc.x + 6.0, dc.y + 7.0), 2.0, theme.text_primary);
        if dl_active {
            // A small accent ring tick as a busy hint.
            draw::stroke_circle(scene, t, Point::new(dl.x1 - 7.0, dl.y0 + 7.0), 4.0, 2.0, theme.accent_primary);
        }
        self.click_targets.push((dl, Hit::OfflineOpen));
    }

    /// The offline-download + manage-cache overlay: a centred card with a
    /// zoom-depth stepper, an estimated-tile/progress readout, and
    /// download / clear-cache / close actions.
    fn draw_offline_overlay(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let c = &theme.tokens.colors;
        // Scrim — also swallows background clicks.
        draw::fill_rect(scene, t, self.rect, draw::alpha(Color::BLACK, 0.45));
        self.click_targets.push((self.rect, Hit::OfflineClose));

        let w = 420.0_f64.min(self.rect.width() - 40.0);
        let h = 318.0_f64;
        let cx = (self.rect.x0 + self.rect.x1) / 2.0;
        let cy = (self.rect.y0 + self.rect.y1) / 2.0;
        let card = Rect::new(cx - w / 2.0, cy - h / 2.0, cx + w / 2.0, cy + h / 2.0);
        draw::card(scene, t, card, 16.0, c.surfaces.raised, c.border_standard);
        // Swallow clicks on the card itself.
        self.click_targets.push((card, Hit::Consume));

        let x = card.x0 + 22.0;
        let right = card.x1 - 22.0;
        let mut y = card.y0 + 20.0;
        draw::text_w(scene, tr, t, "Offline maps", x, y, 18.0, theme.text_primary, TextWeight::Bold);
        // Close (×).
        let close = Rect::new(right - 26.0, card.y0 + 16.0, right, card.y0 + 42.0);
        draw::text_center(scene, tr, t, "✕", close.x0, close.x1, vmid(close.y0, 26.0, 14.0), 14.0, theme.text_dim);
        self.click_targets.push((close, Hit::OfflineClose));
        y += 30.0;

        draw::text(scene, tr, t, "Download the current map area for navigation without a connection.", x, y, 12.0, theme.text_dim);
        y += 28.0;

        // Zoom-depth stepper.
        let base_z = TileLayer::tile_zoom(self.model.zoom);
        let z0 = base_z;
        let z1 = (base_z + self.offline_zoom_extra).min(19);
        draw::text_w(scene, tr, t, "Detail (zoom levels)", x, y, 13.0, theme.text_primary, TextWeight::Semibold);
        let minus = Rect::new(right - 84.0, y - 4.0, right - 58.0, y + 22.0);
        let plus = Rect::new(right - 26.0, y - 4.0, right, y + 22.0);
        draw::fill_rrect(scene, t, minus, 7.0, c.surfaces.base);
        draw::fill_rrect(scene, t, plus, 7.0, c.surfaces.base);
        draw::text_center(scene, tr, t, "–", minus.x0, minus.x1, vmid(minus.y0, 26.0, 18.0), 18.0, theme.text_primary);
        draw::text_center(scene, tr, t, "+", plus.x0, plus.x1, vmid(plus.y0, 26.0, 18.0), 18.0, theme.text_primary);
        draw::text_center(scene, tr, t, &format!("z{z0}–z{z1}"), minus.x1, plus.x0, vmid(y - 4.0, 26.0, 13.0), 13.0, theme.accent_primary);
        self.click_targets.push((minus, Hit::OfflineZoomDelta(-1)));
        self.click_targets.push((plus, Hit::OfflineZoomDelta(1)));
        y += 34.0;

        // Estimate / progress readout.
        let bounds = self.viewport_bounds();
        let est = TileLayer::estimate_download(bounds, z0, z1);
        let prog = self.tiles.progress();
        let readout = if prog.is_active() {
            format!("Downloading {}/{} tiles…", prog.done, prog.total)
        } else if prog.total > 0 && prog.done >= prog.total {
            let failed = if prog.failed > 0 { format!(" ({} failed)", prog.failed) } else { String::new() };
            format!("Downloaded {} tiles{failed}", prog.total)
        } else {
            format!("≈ {est} tiles ({})", fmt_bytes(est as u64 * 18_000))
        };
        draw::text(scene, tr, t, &readout, x, y, 12.0, theme.text_dim);
        y += 22.0;

        // Progress bar.
        let bar = Rect::new(x, y, right, y + 8.0);
        draw::fill_rrect(scene, t, bar, 4.0, c.surfaces.base);
        let frac = if prog.total > 0 { prog.fraction() } else { 0.0 };
        if frac > 0.0 {
            let fill_w = (bar.width() * frac).max(4.0);
            draw::fill_rrect(scene, t, Rect::new(bar.x0, bar.y0, bar.x0 + fill_w, bar.y1), 4.0, theme.accent_primary);
        }
        y += 24.0;

        // Cache size line.
        draw::text(scene, tr, t, &format!("Cached on disk: {}", fmt_bytes(self.cache_bytes)), x, y, 12.0, theme.text_primary);
        y += 30.0;

        // Action buttons: Download (primary) + Clear cache.
        let bh = 38.0;
        let gap = 10.0;
        let bw = (right - x - gap) / 2.0;
        let dl = Rect::new(x, y, x + bw, y + bh);
        let clr = Rect::new(x + bw + gap, y, right, y + bh);
        draw::fill_rrect(scene, t, dl, 9.0, theme.accent_primary);
        draw::text_center(scene, tr, t, "Download this area", dl.x0, dl.x1, vmid(dl.y0, bh, 13.0), 13.0, Color::WHITE);
        self.click_targets.push((dl, Hit::OfflineDownload));
        draw::fill_rrect(scene, t, clr, 9.0, c.surfaces.base);
        draw::stroke_rrect(scene, t, clr, 9.0, 1.0, c.border_standard);
        draw::text_center(scene, tr, t, "Clear cache", clr.x0, clr.x1, vmid(clr.y0, bh, 13.0), 13.0, c.semantic.danger);
        self.click_targets.push((clr, Hit::OfflineClearCache));
    }

    fn draw_status(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        // Only surface transient/loading status as a top-centre pill — steady
        // attribution lives in the bottom bar, so we don't duplicate it here.
        if self.pending == 0 && !self.model.status.contains('…') {
            return;
        }
        let label = &self.model.status;
        if label.is_empty() {
            return;
        }
        let size = 12.0_f32;
        let w = draw::measure_w(tr, label, size) + 36.0;
        let cx = (self.rect.x0 + self.rect.x1) / 2.0;
        let r = Rect::new(cx - w / 2.0, self.rect.y0 + MARGIN, cx + w / 2.0, self.rect.y0 + MARGIN + 30.0);
        draw::card(scene, t, r, 15.0, theme.tokens.colors.surfaces.raised, theme.tokens.colors.border_standard);
        // Small spinner dot.
        let dot = Point::new(r.x0 + 16.0, r.center().y);
        draw::fill_circle(scene, t, dot, 4.0, theme.accent_primary);
        draw::text(scene, tr, t, label, r.x0 + 28.0, vmid(r.y0, r.height(), size), size, theme.text_primary);
    }

    fn draw_panel(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let pr = self.panel_rect();
        if pr.height() < 80.0 {
            return;
        }
        let c = &theme.tokens.colors;
        draw::card(scene, t, pr, 14.0, c.surfaces.raised, c.border_standard);
        // Swallow background clicks.
        self.click_targets.push((pr, Hit::Consume));

        // Segmented tabs.
        let inset = 10.0;
        let tabs_rect = Rect::new(pr.x0 + inset, pr.y0 + inset, pr.x1 - inset, pr.y0 + inset + TAB_H);
        draw::fill_rrect(scene, t, tabs_rect, 9.0, c.surfaces.base);
        let seg_w = tabs_rect.width() / 3.0;
        for (i, p) in Panel::ALL.iter().enumerate() {
            let x0 = tabs_rect.x0 + i as f64 * seg_w;
            let seg = Rect::new(x0, tabs_rect.y0, x0 + seg_w, tabs_rect.y1);
            let active = self.model.panel == *p;
            if active {
                draw::fill_rrect(scene, t, seg.inflate(-3.0, -3.0), 7.0, theme.accent_primary);
            }
            let col = if active { Color::WHITE } else { theme.text_dim };
            draw::text_center(scene, tr, t, p.label(), seg.x0, seg.x1, vmid(seg.y0, seg.height(), 13.0), 13.0, col);
            self.click_targets.push((seg, Hit::Tab(*p)));
        }

        // Content region (below tabs), clipped + scrolled.
        let content = Rect::new(pr.x0 + inset, tabs_rect.y1 + 10.0, pr.x1 - inset, pr.y1 - 10.0);
        self.content_view_h = content.height();
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &content);
        let cy = content.y0 - self.scroll;
        let drawn = match self.model.panel {
            Panel::Explore => self.draw_explore(scene, t, tr, theme, content, cy),
            Panel::Directions => self.draw_directions(scene, t, tr, theme, content, cy),
            Panel::Transit => self.draw_transit(scene, t, tr, theme, content, cy),
        };
        scene.pop_layer();
        self.content_h = drawn - content.y0 + self.scroll;
    }

    // Returns the y of the bottom of the drawn content (absolute).
    fn draw_explore(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        content: Rect,
        mut y: f64,
    ) -> f64 {
        let c = &theme.tokens.colors;
        let x = content.x0;
        let right = content.x1;

        // Selected place card.
        if let Some(sel) = self.model.selected_place().cloned() {
            let card_h = 132.0;
            let cr = Rect::new(x, y, right, y + card_h);
            draw::fill_rrect(scene, t, cr, 12.0, c.surfaces.base);
            // Category swatch.
            let cat_col = canvas::category_color(theme, sel.category);
            draw::fill_rrect(scene, t, Rect::new(cr.x0, cr.y0, cr.x0 + 5.0, cr.y1), 12.0, cat_col);
            draw::text_w(scene, tr, t, &sel.name, x + 16.0, y + 12.0, 18.0, theme.text_primary, TextWeight::Bold);
            let mut meta = sel.category.label().to_string();
            if let Some(rating) = sel.rating {
                meta = format!("{meta}  ·  ★ {rating:.1}");
            }
            draw::text(scene, tr, t, &meta, x + 16.0, y + 38.0, 12.0, theme.accent_primary);
            draw::text_clip(scene, tr, t, &sel.address, x + 16.0, y + 58.0, 12.0, theme.text_dim, right - x - 28.0);

            // Action buttons row.
            let by = y + 86.0;
            let bw = (right - x - 16.0 - 16.0) / 3.0;
            let bh = 34.0;
            let saved = sel.saved;
            let actions: [(&str, Hit, bool); 3] = [
                ("Directions", Hit::Directions, true),
                ("Transit", Hit::Transit, false),
                (if saved { "Saved ✓" } else { "Save" }, Hit::ToggleSave, false),
            ];
            for (i, (label, hit, primary)) in actions.into_iter().enumerate() {
                let bx = x + 8.0 + i as f64 * (bw + 4.0);
                let br = Rect::new(bx, by, bx + bw, by + bh);
                let (fill, fg) = if primary {
                    (theme.accent_primary, Color::WHITE)
                } else if matches!(hit, Hit::ToggleSave) && saved {
                    (draw::alpha(theme.accent_primary, 0.18), theme.accent_primary)
                } else {
                    (c.surfaces.raised, theme.text_primary)
                };
                draw::fill_rrect(scene, t, br, 8.0, fill);
                if !primary {
                    draw::stroke_rrect(scene, t, br, 8.0, 1.0, c.border_standard);
                }
                draw::text_center(scene, tr, t, label, br.x0, br.x1, vmid(br.y0, bh, 12.0), 12.0, fg);
                self.click_targets.push((br, hit));
            }
            y += card_h + 14.0;
        }

        // Results / recents / saved sections.
        let results = self.model.results.clone();
        if !results.is_empty() {
            draw::text_w(scene, tr, t, "RESULTS", x, y, 11.0, theme.text_dim, TextWeight::Semibold);
            y += 20.0;
            for p in &results {
                y = self.draw_place_row(scene, t, tr, theme, content, y, p);
            }
            y += 8.0;
        }

        let saved = self.model.saved_places().into_iter().cloned().collect::<Vec<_>>();
        if !saved.is_empty() {
            draw::text_w(scene, tr, t, "SAVED", x, y, 11.0, theme.text_dim, TextWeight::Semibold);
            y += 20.0;
            for p in &saved {
                y = self.draw_place_row(scene, t, tr, theme, content, y, p);
            }
            y += 8.0;
        }

        if !self.model.recents.is_empty() {
            draw::text_w(scene, tr, t, "RECENT SEARCHES", x, y, 11.0, theme.text_dim, TextWeight::Semibold);
            y += 20.0;
            let recents = self.model.recents.clone();
            for q in &recents {
                let rr = Rect::new(x, y, right, y + 30.0);
                draw::stroke_circle(scene, t, Point::new(x + 11.0, rr.center().y), 5.0, 1.5, theme.text_dim);
                draw::text(scene, tr, t, q, x + 26.0, vmid(rr.y0, 30.0, 12.0), 12.0, theme.text_primary);
                self.click_targets.push((rr, Hit::Recent(q.clone())));
                y += 32.0;
            }
        }
        y
    }

    fn draw_place_row(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        content: Rect,
        y: f64,
        p: &crate::model::Place,
    ) -> f64 {
        let c = &theme.tokens.colors;
        let r = Rect::new(content.x0, y, content.x1, y + ROW_H - 8.0);
        let selected = self.model.selected.as_deref() == Some(p.id.as_str());
        if selected {
            draw::fill_rrect(scene, t, r, 8.0, draw::alpha(theme.accent_primary, 0.14));
        }
        let cat_col = canvas::category_color(theme, p.category);
        draw::fill_circle(scene, t, Point::new(r.x0 + 18.0, r.center().y), 9.0, cat_col);
        draw::fill_circle(scene, t, Point::new(r.x0 + 18.0, r.center().y), 3.2, draw::mix(c.surfaces.base, Color::WHITE, 0.7));
        let tx = r.x0 + 36.0;
        draw::text_clip(scene, tr, t, &p.name, tx, r.y0 + 6.0, 13.0, theme.text_primary, r.x1 - tx - 50.0);
        let sub = format!("{} · {}", p.category.label(), p.address);
        draw::text_clip(scene, tr, t, &sub, tx, r.y0 + 24.0, 11.0, theme.text_dim, r.x1 - tx - 50.0);
        if let Some(rt) = p.rating {
            draw::text_right(scene, tr, t, &format!("★ {rt:.1}"), r.x1 - 8.0, r.y0 + 6.0, 11.0, theme.accent_primary);
        }
        if p.saved {
            draw::text_right(scene, tr, t, "♥", r.x1 - 8.0, r.y0 + 24.0, 12.0, theme.tokens.colors.semantic.danger);
        }
        self.click_targets.push((r, Hit::SelectPlace(p.id.clone())));
        y + ROW_H
    }

    fn draw_directions(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        content: Rect,
        mut y: f64,
    ) -> f64 {
        let c = &theme.tokens.colors;
        let x = content.x0;
        let right = content.x1;

        // Endpoints summary (origin → destination), with a swap button.
        let from = self.model.origin_name.clone();
        let to = self.model.selected_place().map(|p| p.name.clone()).unwrap_or_else(|| "Pick a destination".to_string());
        let ep_h = 64.0;
        let ep = Rect::new(x, y, right, y + ep_h);
        draw::fill_rrect(scene, t, ep, 10.0, c.surfaces.base);
        draw::fill_circle(scene, t, Point::new(x + 16.0, y + 20.0), 4.0, theme.accent_primary);
        draw::fill_circle(scene, t, Point::new(x + 16.0, y + 44.0), 4.0, c.semantic.danger);
        draw::line(scene, t, Point::new(x + 16.0, y + 24.0), Point::new(x + 16.0, y + 40.0), 1.5, theme.text_dim);
        draw::text_clip(scene, tr, t, &from, x + 30.0, y + 12.0, 13.0, theme.text_primary, right - x - 80.0);
        draw::text_clip(scene, tr, t, &to, x + 30.0, y + 36.0, 13.0, theme.text_primary, right - x - 80.0);
        let swap = Rect::new(right - 40.0, y + 14.0, right - 8.0, y + ep_h - 14.0);
        draw::fill_rrect(scene, t, swap, 7.0, c.surfaces.raised);
        draw::text_center(scene, tr, t, "⇅", swap.x0, swap.x1, vmid(swap.y0, swap.height(), 16.0), 16.0, theme.text_primary);
        self.click_targets.push((swap, Hit::SwapEnds));
        y += ep_h + 12.0;

        // Mode segmented control.
        let mode_rect = Rect::new(x, y, right, y + 40.0);
        draw::fill_rrect(scene, t, mode_rect, 9.0, c.surfaces.base);
        let seg_w = mode_rect.width() / 3.0;
        for (i, m) in TravelMode::ALL.iter().enumerate() {
            let sx = mode_rect.x0 + i as f64 * seg_w;
            let seg = Rect::new(sx, mode_rect.y0, sx + seg_w, mode_rect.y1);
            let active = self.model.mode == *m;
            if active {
                draw::fill_rrect(scene, t, seg.inflate(-3.0, -3.0), 7.0, theme.accent_primary);
            }
            let fg = if active { Color::WHITE } else { theme.text_dim };
            draw::text_center(scene, tr, t, m.label(), seg.x0, seg.x1, vmid(seg.y0, seg.height(), 13.0), 13.0, fg);
            self.click_targets.push((seg, Hit::Mode(*m)));
        }
        y += 52.0;

        // Route summary + steps.
        if let Some(route) = self.model.route.clone() {
            let big = geo::fmt_duration(route.duration_s);
            draw::text_w(scene, tr, t, &big, x, y, 26.0, theme.accent_primary, TextWeight::Bold);
            let dist = geo::fmt_distance(route.distance_m);
            let bw = draw::measure_w(tr, &big, 26.0);
            draw::text(scene, tr, t, &format!("· {dist}"), x + bw + 12.0, y + 8.0, 14.0, theme.text_dim);
            // Clear route link.
            let clr = Rect::new(right - 64.0, y + 2.0, right, y + 28.0);
            draw::text_right(scene, tr, t, "Clear", right - 4.0, y + 8.0, 12.0, theme.tokens.colors.semantic.danger);
            self.click_targets.push((clr, Hit::ClearRoute));
            y += 40.0;

            draw::line(scene, t, Point::new(x, y), Point::new(right, y), 1.0, c.border_standard);
            y += 12.0;

            for (i, step) in route.steps.iter().enumerate() {
                let rh = 46.0;
                // Glyph badge.
                let bc = Point::new(x + 16.0, y + rh / 2.0 - 4.0);
                draw::fill_circle(scene, t, bc, 13.0, draw::alpha(theme.accent_primary, 0.16));
                draw::text_center(scene, tr, t, step.maneuver.glyph(), bc.x - 13.0, bc.x + 13.0, bc.y - 9.0, 15.0, theme.accent_primary);
                draw::text_clip(scene, tr, t, &step.instruction, x + 38.0, y, 13.0, theme.text_primary, right - x - 80.0);
                if step.distance_m >= 1.0 {
                    draw::text_right(scene, tr, t, &geo::fmt_distance(step.distance_m), right - 4.0, y + 18.0, 11.0, theme.text_dim);
                }
                // Connector line between steps.
                if i + 1 < route.steps.len() {
                    draw::line(scene, t, Point::new(bc.x, bc.y + 13.0), Point::new(bc.x, y + rh + 9.0), 1.5, draw::alpha(theme.accent_primary, 0.4));
                }
                y += rh;
            }
        } else {
            draw::text(scene, tr, t, "Select a place, then tap Directions.", x, y, 13.0, theme.text_dim);
            y += 24.0;
        }
        y
    }

    fn draw_transit(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        content: Rect,
        mut y: f64,
    ) -> f64 {
        let c = &theme.tokens.colors;
        let x = content.x0;
        let right = content.x1;
        let to = self.model.selected_place().map(|p| p.name.clone()).unwrap_or_else(|| "destination".to_string());
        draw::text_w(scene, tr, t, &format!("Departures to {to}"), x, y, 14.0, theme.text_primary, TextWeight::Semibold);
        y += 26.0;

        if self.model.transit.is_empty() {
            draw::text(scene, tr, t, "No itineraries — pick a destination.", x, y, 13.0, theme.text_dim);
            return y + 24.0;
        }

        let journeys = self.model.transit.clone();
        for (i, j) in journeys.iter().enumerate() {
            let selected = self.model.transit_choice == i;
            let base_h = 86.0;
            let jr = Rect::new(x, y, right, y + base_h);
            draw::fill_rrect(scene, t, jr, 10.0, if selected { draw::alpha(theme.accent_primary, 0.12) } else { c.surfaces.base });
            if selected {
                draw::stroke_rrect(scene, t, jr, 10.0, 1.5, theme.accent_primary);
            }
            // Header: dep–arr  ·  duration  ·  transfers.
            draw::text_w(scene, tr, t, &format!("{} – {}", j.dep, j.arr), x + 12.0, y + 10.0, 15.0, theme.text_primary, TextWeight::Bold);
            draw::text_right(scene, tr, t, &format!("{} min", j.duration_min), right - 12.0, y + 10.0, 14.0, theme.accent_primary);
            let xfer = if j.transfers == 0 { "direct".to_string() } else { format!("{} change{}", j.transfers, if j.transfers == 1 { "" } else { "s" }) };
            draw::text_right(scene, tr, t, &xfer, right - 12.0, y + 30.0, 11.0, theme.text_dim);

            // Leg pills.
            let mut px = x + 12.0;
            let py = y + 38.0;
            for (li, leg) in j.legs.iter().enumerate() {
                if li > 0 {
                    draw::text(scene, tr, t, "›", px, py + 2.0, 13.0, theme.text_dim);
                    px += 14.0;
                }
                let col = transit_color(theme, leg.mode);
                if leg.mode == TransitMode::Walk {
                    // Walk shown as a dashed segment with minutes.
                    let lbl = format!("walk {}m", leg.duration_min.max(1));
                    let w = draw::measure_w(tr, &lbl, 11.0) + 6.0;
                    draw::text(scene, tr, t, &lbl, px, py + 3.0, 11.0, theme.text_dim);
                    px += w + 4.0;
                } else {
                    let lbl = if leg.line.is_empty() { leg.mode.label().to_string() } else { leg.line.clone() };
                    let w = draw::measure_w(tr, &lbl, 12.0) + 16.0;
                    let pill = Rect::new(px, py, px + w, py + 22.0);
                    draw::fill_rrect(scene, t, pill, 6.0, col);
                    draw::text_center(scene, tr, t, &lbl, pill.x0, pill.x1, vmid(pill.y0, 22.0, 12.0), 12.0, Color::WHITE);
                    px += w + 6.0;
                }
                if px > right - 40.0 {
                    break;
                }
            }
            self.click_targets.push((jr, Hit::Journey(i)));
            y += base_h + 10.0;

            // Expanded leg detail for the selected journey.
            if selected {
                for leg in &j.legs {
                    let lh = 34.0;
                    let col = transit_color(theme, leg.mode);
                    draw::line(scene, t, Point::new(x + 18.0, y + 4.0), Point::new(x + 18.0, y + lh), 2.0, col);
                    draw::fill_circle(scene, t, Point::new(x + 18.0, y + 8.0), 4.0, col);
                    let head = if leg.line.is_empty() {
                        format!("Walk · {} min", leg.duration_min.max(1))
                    } else {
                        format!("{} {} {}", leg.mode.label(), leg.line, leg.headsign)
                    };
                    draw::text_clip(scene, tr, t, head.trim(), x + 32.0, y, 12.0, theme.text_primary, right - x - 90.0);
                    let sub = if leg.line.is_empty() {
                        format!("to {}", leg.to_name)
                    } else {
                        format!("{} → {} · {} stops", leg.from_name, leg.to_name, leg.num_stops)
                    };
                    draw::text_clip(scene, tr, t, &sub, x + 32.0, y + 16.0, 11.0, theme.text_dim, right - x - 90.0);
                    draw::text_right(scene, tr, t, &format!("{}–{}", leg.dep, leg.arr), right - 8.0, y + 4.0, 11.0, theme.text_dim);
                    y += lh + 2.0;
                }
                y += 6.0;
            }
        }
        y
    }
}

// ===========================================================================
// Widget impl
// ===========================================================================
impl Widget for MapsApp {
    fn sync(&mut self, state: &mut AutoCommit, _path: &ObjId) {
        // Pull any background network results into the model first.
        self.drain_net();
        // Fold in any tiles / download progress the worker finished. This is a
        // non-blocking try_recv drain — it never waits on the network.
        let before = self.tiles.progress();
        self.tiles.drain();
        let after = self.tiles.progress();
        if before != after && !after.is_active() && after.total > 0 {
            // A download just finished — refresh the on-disk size readout.
            self.cache_bytes = self.tiles.cache_bytes();
            if self.model.status.contains("Downloading") {
                self.model.status = format!("Offline area ready ({} tiles)", after.total);
            }
        }

        if !self.loaded {
            if let Some(loaded) = MapsDoc::load(state) {
                // Adopt an already-seeded (possibly peer-provided) document,
                // but keep our generated basemap.
                self.model = loaded;
            } else {
                MapsDoc::save(state, &self.model);
            }
            self.loaded = true;
            self.dirty = false;
        } else if self.dirty {
            MapsDoc::save(state, &self.model);
            self.dirty = false;
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::percent(1.0),
                },
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        self.node = Some(node);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + layout.location.x as f64,
            origin.y + layout.location.y as f64,
            origin.x + layout.location.x as f64 + layout.size.width as f64,
            origin.y + layout.location.y as f64 + layout.size.height as f64,
        );
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        self.draw_app(scene, transform, text_renderer, theme);
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        _state: &mut AutoCommit,
        _path: &ObjId,
        handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let p = Point::new(position.x / scale_factor, position.y / scale_factor);
                self.last_mouse = p;
                if self.dragging && self.left_down {
                    let dx = p.x - self.drag_last.x;
                    let dy = p.y - self.drag_last.y;
                    if dx.abs() > 0.5 || dy.abs() > 0.5 {
                        self.drag_moved = true;
                        self.pan_by(dx, dy);
                    }
                    self.drag_last = p;
                }
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                let p = self.last_mouse;
                self.left_down = true;
                self.drag_moved = false;
                // Hit-test recorded UI targets (topmost = last drawn).
                let mut consumed = false;
                if let Some(hit) = self
                    .click_targets
                    .iter()
                    .rev()
                    .find(|(r, _)| r.contains(p))
                    .map(|(_, h)| h.clone())
                {
                    // Clicking outside the search bar unfocuses it.
                    if !matches!(hit, Hit::FocusSearch) {
                        self.search_focused = false;
                    }
                    self.dispatch(&hit);
                    consumed = true;
                    *handled = true;
                }
                if !consumed {
                    self.search_focused = false;
                    // Map interaction: marker select or begin pan.
                    if let Some(id) = self.marker_at(p) {
                        self.select_place(&id);
                        *handled = true;
                    } else {
                        self.dragging = true;
                        self.drag_last = p;
                    }
                }
            }
            WindowEvent::MouseInput { state: ElementState::Released, button: MouseButton::Left, .. } => {
                self.left_down = false;
                self.dragging = false;
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => *y as f64,
                    MouseScrollDelta::PixelDelta(pos) => pos.y / 40.0,
                };
                let over_panel = self.panel_rect().contains(self.last_mouse)
                    && self.last_mouse.y > self.panel_rect().y0 + 50.0;
                if over_panel {
                    let max = (self.content_h - self.content_view_h).max(0.0);
                    self.scroll = (self.scroll - dy * 36.0).clamp(0.0, max);
                } else {
                    self.zoom_by(dy * 0.35, self.last_mouse);
                }
                *handled = true;
            }
            WindowEvent::KeyboardInput { event: key_event, .. } => {
                if key_event.state != ElementState::Pressed {
                    return;
                }
                if self.handle_key(&key_event.logical_key, modifiers) {
                    *handled = true;
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &str {
        "MapsApp"
    }

    fn test_id(&self) -> Option<&str> {
        Some("maps_app")
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn needs_idle_sync(&self) -> bool {
        // Keep idling while anything is outstanding: net requests, the initial
        // load, in-flight tiles, or an active offline download. This lets the
        // app fold in worker results and repaint as imagery arrives without the
        // user touching the input device.
        self.pending > 0
            || !self.loaded
            || self.tiles.has_inflight()
            || self.tiles.progress().is_active()
    }

    fn needs_animation_frame(&self) -> bool {
        self.pending > 0
            || self.search_focused
            || self.tiles.has_inflight()
            || self.tiles.progress().is_active()
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

impl MapsApp {
    fn handle_key(&mut self, key: &Key, modifiers: &winit::event::Modifiers) -> bool {
        // Search field has keyboard focus.
        if self.search_focused {
            match key {
                Key::Named(NamedKey::Escape) => {
                    self.search_focused = false;
                    true
                }
                Key::Named(NamedKey::Enter) => {
                    self.search_focused = false;
                    self.run_search();
                    true
                }
                Key::Named(NamedKey::Backspace) => {
                    self.model.query.pop();
                    self.mark_dirty();
                    true
                }
                Key::Named(NamedKey::Space) => {
                    self.model.query.push(' ');
                    self.mark_dirty();
                    true
                }
                Key::Character(s) => {
                    self.model.query.push_str(s);
                    self.mark_dirty();
                    true
                }
                _ => false,
            }
        } else {
            let ctrl = modifiers.state().control_key() || modifiers.state().super_key();
            match key {
                Key::Character(s) if s.as_str() == "/" => {
                    self.search_focused = true;
                    true
                }
                Key::Character(s) if s.as_str() == "+" || s.as_str() == "=" => {
                    self.zoom_by(1.0, self.view_center());
                    true
                }
                Key::Character(s) if s.as_str() == "-" || s.as_str() == "_" => {
                    self.zoom_by(-1.0, self.view_center());
                    true
                }
                Key::Character(s) if s.eq_ignore_ascii_case("d") => {
                    self.model.panel = Panel::Directions;
                    self.recompute_route();
                    true
                }
                Key::Character(s) if s.eq_ignore_ascii_case("t") => {
                    self.recompute_transit();
                    true
                }
                Key::Character(s) if s.eq_ignore_ascii_case("f") => {
                    self.fit_route();
                    true
                }
                Key::Character(s) if s.eq_ignore_ascii_case("s") && !ctrl => {
                    self.toggle_save_selected();
                    true
                }
                Key::Character(s) if s.as_str() == "1" => {
                    self.set_panel(Panel::Explore);
                    true
                }
                Key::Character(s) if s.as_str() == "2" => {
                    self.set_panel(Panel::Directions);
                    true
                }
                Key::Character(s) if s.as_str() == "3" => {
                    self.set_panel(Panel::Transit);
                    true
                }
                Key::Named(NamedKey::ArrowLeft) => {
                    self.pan_by(-80.0, 0.0);
                    true
                }
                Key::Named(NamedKey::ArrowRight) => {
                    self.pan_by(80.0, 0.0);
                    true
                }
                Key::Named(NamedKey::ArrowUp) => {
                    self.pan_by(0.0, -80.0);
                    true
                }
                Key::Named(NamedKey::ArrowDown) => {
                    self.pan_by(0.0, 80.0);
                    true
                }
                Key::Named(NamedKey::Escape) => {
                    if self.offline_open {
                        self.offline_open = false;
                    } else {
                        self.model.selected = None;
                        self.mark_dirty();
                    }
                    true
                }
                Key::Character(s) if s.eq_ignore_ascii_case("m") => {
                    self.offline_open = !self.offline_open;
                    if self.offline_open {
                        self.cache_bytes = self.tiles.cache_bytes();
                    }
                    true
                }
                _ => false,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app_at(w: f64, h: f64) -> MapsApp {
        // Offline variant: no tile worker thread, no network — tests stay
        // hermetic and never touch the wire.
        let mut a = MapsApp::offline();
        a.rect = Rect::new(0.0, 0.0, w, h);
        a
    }

    #[test]
    fn pan_changes_center() {
        let mut a = app_at(1280.0, 800.0);
        let before = a.model.center;
        a.pan_by(120.0, 0.0);
        assert!((a.model.center.lon - before.lon).abs() > 1e-6);
    }

    #[test]
    fn zoom_clamps() {
        let mut a = app_at(1280.0, 800.0);
        for _ in 0..50 {
            a.zoom_by(1.0, a.view_center());
        }
        assert!(a.model.zoom <= 19.0);
        for _ in 0..80 {
            a.zoom_by(-1.0, a.view_center());
        }
        assert!(a.model.zoom >= 3.0);
    }

    #[test]
    fn local_search_selects_match() {
        let mut a = app_at(1280.0, 800.0);
        a.model.query = "viktualien".to_string();
        a.run_search();
        assert_eq!(a.model.selected.as_deref(), Some("viktualienmarkt"));
        assert!(a.model.recents.iter().any(|r| r.eq_ignore_ascii_case("viktualien")));
    }

    #[test]
    fn keyword_search_finds_cafe() {
        let mut a = app_at(1280.0, 800.0);
        a.model.query = "coffee".to_string();
        a.run_search();
        assert!(a.model.results.iter().any(|p| p.category == Category::Cafe));
    }

    #[test]
    fn mode_switch_recomputes_route() {
        let mut a = app_at(1280.0, 800.0);
        a.model.selected = Some("marienplatz".to_string());
        a.set_mode(TravelMode::Foot);
        let route = a.model.route.expect("route present");
        assert_eq!(route.mode, TravelMode::Foot);
    }

    #[test]
    fn marker_hit_test_selects_when_close() {
        let mut a = app_at(1280.0, 800.0);
        a.model.selected = None;
        let proj = a.projection();
        // Project a known place to where its pin head sits.
        let place = a.model.place("hofgarten").unwrap().clone();
        let anchor = proj.project(place.pos);
        let head = Point::new(anchor.x, anchor.y - 22.0);
        let hit = a.marker_at(head);
        assert_eq!(hit.as_deref(), Some("hofgarten"));
    }
}
