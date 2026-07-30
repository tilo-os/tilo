//! The map layer: projects and paints the stylised vector basemap, the
//! active route polyline, and the place markers into a Vello scene.
//!
//! Colours derive from the `Theme` via [`MapPalette`] so the cartography
//! tracks the OS theme (dark map under a dark theme, etc.) instead of
//! hard-coding a palette.

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{BlendMode, Color};

use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;

use crate::draw;
use crate::geo::{self, Projection};
use crate::model::{Category, MapModel, Poi, TransitJourney, TransitMode};
use crate::sample::{Area, AreaKind, Basemap, RoadClass};
use crate::tiles::{self, TileLayer};

/// Slippy-map zoom at and above which POI markers upgrade from small category
/// dots to labelled icon markers bearing a category pictogram. Kept here so the
/// draw pass and the app's hit-test agree on the marker size.
pub const POI_ICON_ZOOM: f64 = 16.0;

/// Cartographic palette resolved from the active theme.
#[derive(Clone, Copy)]
pub struct MapPalette {
    pub land: Color,
    pub building: Color,
    pub building_edge: Color,
    pub park: Color,
    pub park_edge: Color,
    pub water: Color,
    pub road_res: Color,
    pub road_res_casing: Color,
    pub road_secondary: Color,
    pub road_primary: Color,
    pub road_label: Color,
    pub route: Color,
    pub route_casing: Color,
    pub marker_ring: Color,
    /// Pre-tiles / first-run loading surface: a calm DARK neutral pulled from
    /// the theme (NOT the near-white cartographic `land`), so the map never
    /// flashes bright before imagery arrives. Only the loading substrate uses
    /// it; the tile land underlay stays light behind the real light OSM tiles.
    pub substrate: Color,
    /// The faint grid-line tint drawn over the loading substrate.
    pub substrate_grid: Color,
    /// Traffic-congestion signal ramp (free → standstill), pulled from the
    /// theme's semantic palette so it tracks the OS colours. A fixed green →
    /// amber → orange → red ramp, the universally-read traffic convention.
    pub traffic_free: Color,
    pub traffic_moderate: Color,
    pub traffic_heavy: Color,
    pub traffic_severe: Color,
    /// Dark casing drawn under a traffic segment so the signal colour reads on
    /// the light cartographic land.
    pub traffic_casing: Color,
}

impl MapPalette {
    pub fn from_theme(theme: &Theme) -> Self {
        // The map is its own light cartographic surface, independent of the
        // (possibly dark) OS chrome — exactly like the light raster OSM tiles it
        // underlays, whose on-map labels/pills already assume a light backdrop.
        // Lightness is fixed light so the default map never reads murky under a
        // dark OS theme; hues still track the theme (water = info, parks =
        // success) and the route/markers track the accent. A genuinely dark map
        // would be a separate cartography variant keyed off a map-style
        // preference — not something a dark chrome theme forces on the map.
        let sem = &theme.tokens.colors.semantic;
        let w = Color::WHITE;
        let k = Color::BLACK;
        // Warm near-white land: nudged off the theme surface toward white so it
        // stays essentially paper-coloured whatever the surface hue is.
        let land = draw::mix(theme.tokens.colors.surfaces.base, w, 0.94);
        Self {
            land,
            building: draw::mix(land, k, 0.10),
            building_edge: draw::mix(land, k, 0.20),
            park: draw::mix(w, sem.success, 0.30),
            park_edge: draw::mix(w, sem.success, 0.45),
            water: draw::mix(w, sem.info, 0.34),
            road_res: w,
            road_res_casing: draw::mix(land, k, 0.16),
            road_secondary: draw::mix(w, sem.warning, 0.28),
            road_primary: draw::mix(w, sem.warning, 0.48),
            road_label: draw::mix(w, k, 0.62),
            route: theme.accent_primary,
            route_casing: draw::mix(theme.accent_primary, w, 0.5),
            marker_ring: w,
            // The loading substrate is a theme surface, not the paper land: dark
            // under a dark theme so a first-run map is a quiet surface, not a
            // bright flash. The grid is a faint step off it toward the raised
            // surface, so it reads as texture without glowing.
            substrate: theme.tokens.colors.surfaces.base,
            substrate_grid: draw::mix(
                theme.tokens.colors.surfaces.base,
                theme.tokens.colors.surfaces.raised,
                0.9,
            ),
            // Saturated signal colours (not white-mixed like the roads) so they
            // pop as an overlay over the light land.
            traffic_free: sem.success,
            traffic_moderate: sem.warning,
            traffic_heavy: draw::mix(sem.warning, sem.danger, 0.55),
            traffic_severe: sem.danger,
            traffic_casing: draw::mix(land, k, 0.42),
        }
    }
}

/// The signal colour for a congestion level, resolved from the palette.
pub fn traffic_color(pal: &MapPalette, level: crate::traffic::Congestion) -> Color {
    use crate::traffic::Congestion::*;
    match level {
        Free => pal.traffic_free,
        Moderate => pal.traffic_moderate,
        Heavy => pal.traffic_heavy,
        Severe => pal.traffic_severe,
    }
}

/// Resolve a marker colour for a POI category from theme tokens.
pub fn category_color(theme: &Theme, cat: Category) -> Color {
    let c = &theme.tokens.colors;
    match cat {
        Category::Landmark => c.semantic.danger,
        Category::Cafe => c.semantic.warning,
        Category::Restaurant => draw::mix(c.semantic.danger, c.semantic.warning, 0.5),
        Category::Bar => c.accent[6],
        Category::Museum => c.semantic.info,
        Category::Park => c.semantic.success,
        Category::Transit => c.accent[ui::tokens::colors::ACCENT_PRIMARY_IDX],
        Category::Hotel => c.accent[7],
        Category::Shop => c.semantic.warning,
        Category::Hospital => c.semantic.danger,
        Category::Other => c.neutral[6],
    }
}

/// Marker draw priority for decluttering — a more salient category wins a
/// contested screen cell, so a dense area keeps its landmarks / transit stops
/// rather than a random scatter of shops.
pub fn category_priority(cat: Category) -> u8 {
    match cat {
        Category::Transit => 6,
        Category::Landmark => 6,
        Category::Hospital => 5,
        Category::Museum => 5,
        Category::Park => 4,
        Category::Hotel => 3,
        Category::Restaurant => 3,
        Category::Cafe => 2,
        Category::Bar => 2,
        Category::Shop => 1,
        Category::Other => 0,
    }
}

/// Choose which POIs to draw so markers don't pile up on top of one another at
/// the current projection. A greedy pass in descending priority order keeps a
/// POI only when no already-kept marker sits within `min_px` screen pixels of
/// it; the `selected` POI (its info card is open) is always kept. Returns the
/// kept indices into `pois` in the original order, so draw order stays stable.
///
/// Pure — the same function backs both the draw pass and the click hit-test, so
/// a click always resolves to exactly the marker the user can see.
pub fn declutter_indices(
    proj: &Projection,
    pois: &[Poi],
    min_px: f64,
    selected: Option<&str>,
) -> Vec<usize> {
    let pts: Vec<Point> = pois.iter().map(|p| proj.project(p.pos)).collect();
    // Consider the selected POI first, then most-salient categories, then input
    // order as a stable tie-break.
    let mut order: Vec<usize> = (0..pois.len()).collect();
    order.sort_by(|&a, &b| {
        let sa = selected == Some(pois[a].id.as_str());
        let sb = selected == Some(pois[b].id.as_str());
        sb.cmp(&sa)
            .then(category_priority(pois[b].category).cmp(&category_priority(pois[a].category)))
            .then(a.cmp(&b))
    });
    let min2 = (min_px * min_px).max(0.0);
    let mut kept_pts: Vec<Point> = Vec::new();
    let mut keep = vec![false; pois.len()];
    for &i in &order {
        let p = pts[i];
        let clear = kept_pts.iter().all(|k| {
            let dx = k.x - p.x;
            let dy = k.y - p.y;
            dx * dx + dy * dy >= min2
        });
        if clear {
            keep[i] = true;
            kept_pts.push(p);
        }
    }
    (0..pois.len()).filter(|&i| keep[i]).collect()
}

/// Project a polyline of geo coords to screen points.
fn project_pts(proj: &Projection, pts: &[crate::model::LatLon]) -> Vec<Point> {
    pts.iter().map(|p| proj.project(*p)).collect()
}

/// Road stroke widths grow with zoom so the map reads at any scale.
fn road_width(class: RoadClass, zoom: f64) -> f64 {
    let k = ((zoom - 12.0).max(0.0)) * 0.35 + 1.0;
    match class {
        RoadClass::Primary => 4.2 * k,
        RoadClass::Secondary => 3.0 * k,
        RoadClass::Residential => 1.6 * k,
        RoadClass::Path => 1.0 * k,
    }
}

/// Paint the OSM raster-tile basemap into `clip`.
///
/// This is the non-blocking heart of the map. For every tile covering the
/// viewport we:
///   1. paint the land colour first (so a never-seen area is never blank),
///   2. paint a cached *ancestor* (lower-z) tile stretched under the hole so a
///      fresh zoom-*in* shows imagery *instantly* instead of waiting on I/O,
///   3. paint cached *descendant* (higher-z) tiles into the hole so a fresh
///      zoom-*out* is filled from the sharper children we already had resident,
///   4. paint the exact tile on top if it is already decoded in memory,
///   5. otherwise *request* it from the background worker and move on.
///
/// Nothing here ever blocks on the network or on image decode — step 5 returns
/// immediately. Because a fresh zoom (in *or* out) reuses the already-loaded
/// neighbouring-zoom tiles scaled under an affine, the map keeps showing
/// imagery through the whole zoom and never blanks mid-transition.
///
/// Returns `true` if *any* imagery was painted — exact, ancestor, or descendant.
/// The caller uses this to decide whether to fall back to the stylised vector
/// basemap / reference-grid substrate: it must only do so when the viewport is
/// genuinely empty (headless `--shots`, or the very first frame offline), never
/// when scaled placeholder tiles are already covering it (which the substrate
/// would otherwise paint straight over — the old mid-zoom blank).
pub fn draw_tiles(
    scene: &mut Scene,
    t: Affine,
    clip: Rect,
    tiles_layer: &mut TileLayer,
    model: &MapModel,
    view_center: Point,
    pal: &MapPalette,
) -> bool {
    let visible = TileLayer::visible_tiles(model.center, model.zoom, view_center, clip);
    // Clip the whole tile pass to the viewport so edge tiles don't bleed.
    scene.push_layer(vello::peniko::Fill::NonZero, BlendMode::default(), 1.0, t, &clip);

    let mut painted_any = false;
    let mut pending: Vec<crate::tiles::TileId> = Vec::new();

    for (id, rect) in &visible {
        // (1) land underlay.
        tiles::paint_placeholder(scene, t, *rect, pal.land);

        let have_exact = tiles_layer.get(*id).is_some();
        if !have_exact {
            // (2) ancestor placeholder (blurry lower-z fill for zoom-in / pan).
            if let Some((anc_id, levels)) = tiles_layer.ancestor_placeholder(*id) {
                if let Some(anc_img) = tiles_layer.get(anc_id).cloned() {
                    tiles::paint_ancestor(scene, t, &anc_img, anc_id, *id, levels, *rect);
                    painted_any = true;
                }
            }
            // (3) descendant placeholder (sharp higher-z fill for zoom-out).
            if tiles::paint_descendants(scene, t, tiles_layer, *id, *rect, 2) {
                painted_any = true;
            }
            pending.push(*id);
        }

        // (4) exact tile on top.
        if let Some(img) = tiles_layer.get(*id).cloned() {
            tiles::paint_tile(scene, t, &img, *rect);
            painted_any = true;
        }
    }
    scene.pop_layer();

    // (5) request the missing tiles — non-blocking; results arrive via drain().
    for id in pending {
        tiles_layer.request(id);
    }

    painted_any
}

/// Paint a reference-grid substrate aligned to the slippy-map tile pyramid.
///
/// Shown when there is neither real raster imagery nor a vector basemap yet —
/// the live app's first frames and any offline gap. Instead of a flat void or a
/// bright white flash, the viewport reads as a calm map substrate: a dark
/// neutral theme surface, a faint tile-boundary grid, and a quarter-tile
/// sub-grid for texture. The grid uses the exact same projection as
/// [`draw_tiles`], so it lines up with the imagery that fades in on top of it.
pub fn draw_grid_substrate(
    scene: &mut Scene,
    t: Affine,
    clip: Rect,
    model: &MapModel,
    view_center: Point,
    pal: &MapPalette,
) {
    draw::fill_rect(scene, t, clip, pal.substrate);
    let tiles = TileLayer::visible_tiles(model.center, model.zoom, view_center, clip);
    let major = draw::mix(pal.substrate, pal.substrate_grid, 0.85);
    let minor = draw::mix(pal.substrate, pal.substrate_grid, 0.4);
    scene.push_layer(vello::peniko::Fill::NonZero, BlendMode::default(), 1.0, t, &clip);
    for (_, rect) in &tiles {
        // Quarter-tile minor grid for a paper-like texture.
        for k in 1..4 {
            let fx = rect.x0 + rect.width() * k as f64 / 4.0;
            let fy = rect.y0 + rect.height() * k as f64 / 4.0;
            draw::line(scene, t, Point::new(fx, rect.y0), Point::new(fx, rect.y1), 0.5, minor);
            draw::line(scene, t, Point::new(rect.x0, fy), Point::new(rect.x1, fy), 0.5, minor);
        }
        // Major tile boundary (top + left edge; neighbours cover the rest).
        draw::line(scene, t, Point::new(rect.x0, rect.y0), Point::new(rect.x0, rect.y1), 1.0, major);
        draw::line(scene, t, Point::new(rect.x0, rect.y0), Point::new(rect.x1, rect.y0), 1.0, major);
    }
    scene.pop_layer();
}

/// Paint the full basemap (areas → river → roads) clipped to `clip`.
pub fn draw_basemap(
    scene: &mut Scene,
    t: Affine,
    clip: Rect,
    proj: &Projection,
    base: &Basemap,
    pal: &MapPalette,
) {
    // Land backdrop.
    draw::fill_rect(scene, t, clip, pal.land);

    // Areas: water + parks first (landuse), then buildings on top.
    for a in base.areas.iter().filter(|a| a.kind == AreaKind::Water) {
        draw_area(scene, t, proj, a, pal.water, None);
    }
    for a in base.areas.iter().filter(|a| a.kind == AreaKind::Park) {
        draw_area(scene, t, proj, a, pal.park, Some(pal.park_edge));
    }

    // River: a thick water-coloured stroke.
    let river_pts = project_pts(proj, &base.river);
    draw::polyline(scene, t, &river_pts, road_width(RoadClass::Primary, proj.zoom()) * 3.2, pal.water);

    for a in base.areas.iter().filter(|a| a.kind == AreaKind::Building) {
        draw_area(scene, t, proj, a, pal.building, Some(pal.building_edge));
    }

    // Roads: casings first (so intersections look layered), then surfaces.
    for r in &base.roads {
        if r.class == RoadClass::Residential {
            continue;
        }
        let pts = project_pts(proj, &r.pts);
        draw::polyline(scene, t, &pts, road_width(r.class, proj.zoom()) + 2.0, pal.road_res_casing);
    }
    for r in &base.roads {
        let pts = project_pts(proj, &r.pts);
        let color = match r.class {
            RoadClass::Primary => pal.road_primary,
            RoadClass::Secondary => pal.road_secondary,
            _ => pal.road_res,
        };
        draw::polyline(scene, t, &pts, road_width(r.class, proj.zoom()), color);
    }
}

fn draw_area(scene: &mut Scene, t: Affine, proj: &Projection, a: &Area, fill: Color, edge: Option<Color>) {
    let pts = project_pts(proj, &a.ring);
    draw::polygon(scene, t, &pts, fill);
    if let Some(e) = edge {
        // Outline by stroking the closed ring.
        let mut ring = pts.clone();
        if let Some(first) = pts.first() {
            ring.push(*first);
        }
        draw::polyline(scene, t, &ring, 1.0, e);
    }
}

/// Paint the active route: a casing + accent line with start/end pucks.
pub fn draw_route(
    scene: &mut Scene,
    t: Affine,
    proj: &Projection,
    route: &crate::model::Route,
    pal: &MapPalette,
) {
    let pts = project_pts(proj, &route.geometry);
    if pts.len() < 2 {
        return;
    }
    draw::polyline(scene, t, &pts, 9.0, pal.route_casing);
    draw::polyline(scene, t, &pts, 5.5, pal.route);

    // Start puck (hollow) + end puck (filled).
    let start = pts[0];
    let end = *pts.last().unwrap();
    draw::fill_circle(scene, t, start, 7.0, pal.marker_ring);
    draw::stroke_circle(scene, t, start, 7.0, 3.0, pal.route);
    draw::fill_circle(scene, t, end, 7.0, pal.route);
    draw::stroke_circle(scene, t, end, 7.0, 2.5, pal.marker_ring);
}

/// Paint the traffic overlay: each road segment as a dark casing under a
/// signal-coloured line (green → red by congestion). Drawn over the basemap /
/// tiles but under the active route + markers so navigation stays legible.
/// Empty data draws nothing — the caller shows the "No traffic data" state.
pub fn draw_traffic(
    scene: &mut Scene,
    t: Affine,
    proj: &Projection,
    traffic: &crate::traffic::TrafficData,
    pal: &MapPalette,
) {
    use crate::traffic::Congestion;
    // Ribbon width scales with zoom (same growth as the roads) and with
    // congestion — free flow is a subtle line, a standstill is a fat one, so
    // the eye is drawn to the jams the way a real traffic layer works.
    let k = ((proj.zoom() - 12.0).max(0.0)) * 0.35 + 1.0;
    let width = |level: Congestion| {
        k * match level {
            Congestion::Free => 2.2,
            Congestion::Moderate => 2.7,
            Congestion::Heavy => 3.1,
            Congestion::Severe => 3.4,
        }
    };
    // Casing pass first (a thin dark outline so the signal fills read on the
    // light land and adjacent ribbons stay distinct), then the coloured fills.
    for seg in &traffic.segments {
        let pts = project_pts(proj, &seg.geometry);
        if pts.len() < 2 {
            continue;
        }
        draw::polyline(scene, t, &pts, width(seg.level) + 2.0, pal.traffic_casing);
    }
    for seg in &traffic.segments {
        let pts = project_pts(proj, &seg.geometry);
        if pts.len() < 2 {
            continue;
        }
        draw::polyline(scene, t, &pts, width(seg.level), traffic_color(pal, seg.level));
    }
}

/// Per-mode colour for a public-transport leg, resolved from theme tokens (no
/// hard-coded RGB). The fallback when a leg carries no authentic feed colour.
pub fn transit_leg_color(theme: &Theme, mode: TransitMode) -> Color {
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

/// The colour to draw a transit leg in: the *authentic* GTFS line colour from
/// the feed (`routeColor`, e.g. the S2's green) when present, otherwise the
/// theme-derived per-mode colour. This is what makes the on-map legs and the
/// panel pills read as the real lines a rider recognises.
pub fn leg_color(theme: &Theme, leg: &crate::model::TransitLeg) -> Color {
    leg.color
        .as_deref()
        .and_then(draw::hex_color)
        .unwrap_or_else(|| transit_leg_color(theme, leg.mode))
}

/// Paint the selected public-transport itinerary onto the map: each transit leg
/// as a mode-coloured line (with a casing), each walking leg as a lighter dashed
/// connector, and a stop puck at every leg boundary (the transfer points).
///
/// The live app fills `TransitLeg.geometry` from the planner's `legGeometry`;
/// the offline sample seeds plausible on-grid polylines. A leg whose geometry is
/// empty is simply skipped, so a partial itinerary still draws what it can.
pub fn draw_transit(
    scene: &mut Scene,
    t: Affine,
    proj: &Projection,
    journey: &TransitJourney,
    theme: &Theme,
    pal: &MapPalette,
    clip: Rect,
) {
    scene.push_layer(vello::peniko::Fill::NonZero, BlendMode::default(), 1.0, t, &clip);
    // Lines first (casings under surfaces), then boundary pucks on top.
    for leg in &journey.legs {
        if leg.geometry.len() < 2 {
            continue;
        }
        let pts = project_pts(proj, &leg.geometry);
        let col = leg_color(theme, leg);
        if leg.mode == TransitMode::Walk {
            // Walking connector: a short dashed line, no casing.
            draw_dashed(scene, t, &pts, 3.0, draw::alpha(col, 0.9), 6.0, 5.0);
        } else {
            draw::polyline(scene, t, &pts, 8.0, draw::mix(col, Color::BLACK, 0.45));
            draw::polyline(scene, t, &pts, 5.0, col);
        }
    }
    // Boundary/transfer pucks between consecutive legs.
    for leg in &journey.legs {
        if leg.mode == TransitMode::Walk {
            continue;
        }
        let pts = project_pts(proj, &leg.geometry);
        let col = leg_color(theme, leg);
        for anchor in [pts.first(), pts.last()].into_iter().flatten() {
            if clip.contains(*anchor) {
                draw::fill_circle(scene, t, *anchor, 5.0, pal.marker_ring);
                draw::stroke_circle(scene, t, *anchor, 5.0, 2.5, col);
            }
        }
    }
    scene.pop_layer();
}

/// Stroke a polyline as evenly-spaced dashes (`dash` on, `gap` off).
fn draw_dashed(scene: &mut Scene, t: Affine, pts: &[Point], width: f64, color: Color, dash: f64, gap: f64) {
    if pts.len() < 2 {
        return;
    }
    let period = dash + gap;
    for seg in pts.windows(2) {
        let (a, b) = (seg[0], seg[1]);
        let len = a.distance(b);
        if len < 1e-3 {
            continue;
        }
        let (dx, dy) = ((b.x - a.x) / len, (b.y - a.y) / len);
        let mut d = 0.0;
        while d < len {
            let s = Point::new(a.x + dx * d, a.y + dy * d);
            let e_len = (d + dash).min(len);
            let e = Point::new(a.x + dx * e_len, a.y + dy * e_len);
            draw::line(scene, t, s, e, width, color);
            d += period;
        }
    }
}

/// Draw all place markers; the selected one is enlarged and labelled.
pub fn draw_markers(
    scene: &mut Scene,
    t: Affine,
    tr: &mut TextRenderer,
    proj: &Projection,
    model: &MapModel,
    theme: &Theme,
    pal: &MapPalette,
    clip: Rect,
) {
    let selected = model.selected.clone();
    // Clip the whole marker layer to the map area so pins and their labels never
    // bleed out from under an overlay drawer (the caller passes the visible map
    // region as `clip` when the narrow drawer is open).
    scene.push_layer(vello::peniko::Fill::NonZero, BlendMode::default(), 1.0, t, &clip);
    // Origin marker, if a route/origin is set.
    if let Some(o) = model.origin {
        let p = proj.project(o);
        if clip.contains(p) {
            draw::fill_circle(scene, t, Point::new(p.x, p.y + 1.0), 6.5, Color::from_rgba8(0, 0, 0, 60));
            draw::fill_circle(scene, t, p, 6.0, theme.tokens.colors.surfaces.base);
            draw::stroke_circle(scene, t, p, 6.0, 3.0, theme.accent_primary);
            draw::fill_circle(scene, t, p, 2.2, theme.accent_primary);
        }
    }

    // Unselected first, so the selected marker paints on top.
    for place in &model.places {
        if Some(&place.id) == selected.as_ref() {
            continue;
        }
        let p = proj.project(place.pos);
        if !clip.contains(p) {
            continue;
        }
        draw_pin(scene, t, p, category_color(theme, place.category), pal, false);
    }

    if let Some(sel) = model.selected_place() {
        let p = proj.project(sel.pos);
        if clip.contains(p) {
            draw_pin(scene, t, p, category_color(theme, sel.category), pal, true);
            // Floating label above the selected pin.
            let label = &sel.name;
            let pad = 7.0;
            let tw = draw::measure_w(tr, label, 12.0);
            let lw = tw + pad * 2.0;
            let lh = 22.0;
            let lx = p.x - lw / 2.0;
            let ly = p.y - 56.0;
            let rect = Rect::new(lx, ly, lx + lw, ly + lh);
            // Only draw the label if it fits inside the map area — otherwise it
            // would be sliced at the drawer/panel edge into an unreadable
            // fragment. The pin still shows; the drawer already names the place.
            if rect.x0 >= clip.x0 && rect.x1 <= clip.x1 {
                draw::card(
                    scene,
                    t,
                    rect,
                    6.0,
                    theme.tokens.colors.surfaces.raised,
                    theme.tokens.colors.border_standard,
                );
                draw::text_w(
                    scene,
                    tr,
                    t,
                    label,
                    lx + pad,
                    ly + 4.0,
                    12.0,
                    theme.text_primary,
                    TextWeight::Semibold,
                );
            }
        }
    }
    scene.pop_layer();
}

/// Draw the live POI marker layer (Overpass results) over the basemap — OUR
/// markers, resolved from the OS theme, never baked into the raster tiles.
///
/// `visible` is the decluttered index set into `pois` (see [`declutter_indices`])
/// so a dense area never piles markers on top of one another. Far out, each POI
/// is a small themed category dot; at [`POI_ICON_ZOOM`] and closer it upgrades to
/// a larger disc bearing a white category pictogram and (past the label
/// threshold) a name plate. The selected POI is enlarged with an accent halo and
/// always labelled.
#[allow(clippy::too_many_arguments)]
pub fn draw_pois(
    scene: &mut Scene,
    t: Affine,
    tr: &mut TextRenderer,
    proj: &Projection,
    pois: &[Poi],
    visible: &[usize],
    selected: Option<&str>,
    theme: &Theme,
    pal: &MapPalette,
    clip: Rect,
) {
    let icons = proj.zoom() >= POI_ICON_ZOOM;
    let show_labels = proj.zoom() >= 16.5;
    scene.push_layer(vello::peniko::Fill::NonZero, BlendMode::default(), 1.0, t, &clip);
    for &i in visible {
        let poi = &pois[i];
        if selected == Some(poi.id.as_str()) {
            continue; // draw the selected POI last, on top
        }
        let p = proj.project(poi.pos);
        if !clip.contains(p) {
            continue;
        }
        draw_poi_dot(scene, t, tr, p, poi, theme, pal, false, show_labels, icons, clip);
    }
    if let Some(sel) = selected {
        if let Some(poi) = pois.iter().find(|p| p.id == sel) {
            let p = proj.project(poi.pos);
            if clip.contains(p) {
                draw_poi_dot(scene, t, tr, p, poi, theme, pal, true, true, icons, clip);
            }
        }
    }
    scene.pop_layer();
}

#[allow(clippy::too_many_arguments)]
fn draw_poi_dot(
    scene: &mut Scene,
    t: Affine,
    tr: &mut TextRenderer,
    p: Point,
    poi: &Poi,
    theme: &Theme,
    pal: &MapPalette,
    selected: bool,
    label: bool,
    icons: bool,
    clip: Rect,
) {
    let col = category_color(theme, poi.category);
    let r = match (icons, selected) {
        (true, true) => 13.0,
        (true, false) => 9.0,
        (false, true) => 8.5,
        (false, false) => 4.5,
    };
    draw::fill_circle(scene, t, Point::new(p.x, p.y + 0.9), r + 1.3, draw::alpha(Color::BLACK, 0.32));
    draw::fill_circle(scene, t, p, r, col);
    draw::stroke_circle(scene, t, p, r, if selected { 2.5 } else { 1.6 }, pal.marker_ring);
    if icons {
        // White category pictogram on the disc; the disc colour shows through
        // any cut-outs (train windows, etc.).
        draw_category_icon(scene, t, p, r, poi.category, pal.marker_ring, col);
    }
    if selected {
        draw::stroke_circle(scene, t, p, r + 4.0, 2.0, draw::alpha(col, 0.5));
    }
    if label {
        let size = if selected { 12.0 } else { 11.0 };
        let tw = draw::measure_w(tr, &poi.name, size);
        let lx = p.x + r + 6.0;
        let ly = p.y - size as f64 * 0.72;
        let plate = Rect::new(lx - 4.0, ly - 1.0, lx + tw + 6.0, ly + size as f64 + 3.0);
        // Only draw the name plate when it fits in the map area, so it is never
        // sliced at the drawer / screen edge into an unreadable fragment. The
        // marker still shows; the info card / list already carries the name.
        if plate.x0 >= clip.x0 && plate.x1 <= clip.x1 && plate.y0 >= clip.y0 {
            draw::fill_rrect(scene, t, plate, 4.0, draw::alpha(theme.tokens.colors.surfaces.base, 0.74));
            draw::text_w(scene, tr, t, &poi.name, lx, ly, size, theme.text_primary, TextWeight::Medium);
        }
    }
}

/// Draw a compact white category pictogram centred at `c` on a disc of radius
/// `r`. `fg` is the icon colour (the near-white marker ring); `bg` is the disc
/// colour, used for cut-outs (e.g. train windows) so they read against the fill.
/// Deliberately built from a handful of primitives — no icon-font dependency, so
/// it renders identically headless and live and tracks the OS theme.
pub fn draw_category_icon(
    scene: &mut Scene,
    t: Affine,
    c: Point,
    r: f64,
    cat: Category,
    fg: Color,
    bg: Color,
) {
    let s = r * 0.58;
    let lw = (r * 0.16).max(1.4);
    let ln = |scene: &mut Scene, ax: f64, ay: f64, bx: f64, by: f64, w: f64| {
        draw::line(scene, t, Point::new(ax, ay), Point::new(bx, by), w, fg);
    };
    match cat {
        Category::Hospital => {
            draw::fill_rrect(scene, t, Rect::new(c.x - s, c.y - s * 0.32, c.x + s, c.y + s * 0.32), 1.5, fg);
            draw::fill_rrect(scene, t, Rect::new(c.x - s * 0.32, c.y - s, c.x + s * 0.32, c.y + s), 1.5, fg);
        }
        Category::Landmark => {
            let mut pts = Vec::with_capacity(10);
            for k in 0..10 {
                let ang = -std::f64::consts::FRAC_PI_2 + k as f64 * std::f64::consts::PI / 5.0;
                let rad = if k % 2 == 0 { s } else { s * 0.45 };
                pts.push(Point::new(c.x + rad * ang.cos(), c.y + rad * ang.sin()));
            }
            draw::polygon(scene, t, &pts, fg);
        }
        Category::Park => {
            ln(scene, c.x, c.y + s * 0.1, c.x, c.y + s * 0.8, lw * 1.4);
            draw::fill_circle(scene, t, Point::new(c.x, c.y - s * 0.2), s * 0.62, fg);
        }
        Category::Cafe => {
            draw::fill_rrect(scene, t, Rect::new(c.x - s * 0.6, c.y - s * 0.35, c.x + s * 0.35, c.y + s * 0.45), 2.0, fg);
            draw::stroke_circle(scene, t, Point::new(c.x + s * 0.5, c.y + s * 0.02), s * 0.3, lw, fg);
            ln(scene, c.x - s * 0.8, c.y + s * 0.68, c.x + s * 0.6, c.y + s * 0.68, lw);
        }
        Category::Restaurant => {
            // Fork (left): three short prongs onto a stem.
            for dx in [-0.68, -0.45, -0.22] {
                ln(scene, c.x + dx * s, c.y - s * 0.7, c.x + dx * s, c.y - s * 0.18, lw);
            }
            ln(scene, c.x - s * 0.45, c.y - s * 0.18, c.x - s * 0.45, c.y + s * 0.82, lw * 1.5);
            // Knife (right): a single tapering blade+handle.
            ln(scene, c.x + s * 0.5, c.y - s * 0.7, c.x + s * 0.5, c.y + s * 0.82, lw * 1.7);
        }
        Category::Bar => {
            let bowl = [
                Point::new(c.x - s * 0.72, c.y - s * 0.55),
                Point::new(c.x + s * 0.72, c.y - s * 0.55),
                Point::new(c.x, c.y + s * 0.18),
            ];
            draw::polygon(scene, t, &bowl, fg);
            ln(scene, c.x, c.y + s * 0.18, c.x, c.y + s * 0.72, lw);
            ln(scene, c.x - s * 0.4, c.y + s * 0.72, c.x + s * 0.4, c.y + s * 0.72, lw);
        }
        Category::Museum => {
            let roof = [
                Point::new(c.x - s * 0.82, c.y - s * 0.18),
                Point::new(c.x + s * 0.82, c.y - s * 0.18),
                Point::new(c.x, c.y - s * 0.78),
            ];
            draw::polygon(scene, t, &roof, fg);
            for dx in [-0.45, 0.0, 0.45] {
                ln(scene, c.x + dx * s, c.y - s * 0.12, c.x + dx * s, c.y + s * 0.55, lw * 1.2);
            }
            ln(scene, c.x - s * 0.78, c.y + s * 0.66, c.x + s * 0.78, c.y + s * 0.66, lw * 1.3);
        }
        Category::Transit => {
            draw::fill_rrect(scene, t, Rect::new(c.x - s * 0.6, c.y - s * 0.72, c.x + s * 0.6, c.y + s * 0.5), 2.5, fg);
            // Two windows cut out of the body (disc colour).
            draw::fill_rrect(scene, t, Rect::new(c.x - s * 0.42, c.y - s * 0.5, c.x - s * 0.06, c.y - s * 0.06), 1.5, bg);
            draw::fill_rrect(scene, t, Rect::new(c.x + s * 0.06, c.y - s * 0.5, c.x + s * 0.42, c.y - s * 0.06), 1.5, bg);
            draw::fill_circle(scene, t, Point::new(c.x - s * 0.32, c.y + s * 0.62), s * 0.16, fg);
            draw::fill_circle(scene, t, Point::new(c.x + s * 0.32, c.y + s * 0.62), s * 0.16, fg);
        }
        Category::Hotel => {
            ln(scene, c.x - s * 0.72, c.y - s * 0.5, c.x - s * 0.72, c.y + s * 0.5, lw * 1.4);
            draw::fill_rrect(scene, t, Rect::new(c.x - s * 0.72, c.y + s * 0.08, c.x + s * 0.78, c.y + s * 0.46), 2.0, fg);
            draw::fill_rrect(scene, t, Rect::new(c.x - s * 0.5, c.y - s * 0.2, c.x - s * 0.08, c.y + s * 0.12), 2.0, fg);
        }
        Category::Shop => {
            draw::stroke_circle(scene, t, Point::new(c.x, c.y - s * 0.22), s * 0.32, lw, fg);
            draw::fill_rrect(scene, t, Rect::new(c.x - s * 0.6, c.y - s * 0.18, c.x + s * 0.6, c.y + s * 0.72), 2.5, fg);
        }
        Category::Other => {
            draw::fill_circle(scene, t, c, s * 0.4, fg);
        }
    }
}

/// Draw a teardrop pin anchored at `anchor`.
fn draw_pin(scene: &mut Scene, t: Affine, anchor: Point, color: Color, pal: &MapPalette, selected: bool) {
    let r = if selected { 13.0 } else { 9.0 };
    let h = if selected { 30.0 } else { 22.0 };
    let center = Point::new(anchor.x, anchor.y - h);

    // Ground shadow.
    let shadow = vello::kurbo::Ellipse::new(anchor, (r * 0.5, r * 0.22), 0.0);
    scene.fill(
        vello::peniko::Fill::NonZero,
        t,
        Color::from_rgba8(0, 0, 0, 70),
        None,
        &shadow,
    );

    // Stem: a triangle from the circle down to the anchor.
    let mut stem = vello::kurbo::BezPath::new();
    stem.move_to(Point::new(center.x - r * 0.55, center.y + r * 0.55));
    stem.line_to(Point::new(center.x + r * 0.55, center.y + r * 0.55));
    stem.line_to(anchor);
    stem.close_path();
    scene.fill(vello::peniko::Fill::NonZero, t, color, None, &stem);

    // Head.
    draw::fill_circle(scene, t, center, r, color);
    draw::stroke_circle(scene, t, center, r, if selected { 3.0 } else { 2.0 }, pal.marker_ring);
    // Inner dot.
    draw::fill_circle(scene, t, center, r * 0.34, pal.marker_ring);

    if selected {
        // Outer accent halo.
        draw::stroke_circle(scene, t, center, r + 4.0, 2.0, draw::alpha(color, 0.5));
    }
}

/// Draw the bottom-right scale bar + attribution into `clip`.
pub fn draw_scale_and_attribution(
    scene: &mut Scene,
    t: Affine,
    tr: &mut TextRenderer,
    clip: Rect,
    model: &MapModel,
    theme: &Theme,
) {
    // Scale bar.
    let (label, px) = geo::scale_bar(model.center, model.zoom, 110.0);
    let y = clip.y1 - 26.0;
    let x0 = clip.x1 - 24.0 - px;
    let x1 = clip.x1 - 24.0;
    let bar_bg = draw::alpha(theme.tokens.colors.surfaces.base, 0.7);
    draw::fill_rrect(
        scene,
        t,
        Rect::new(x0 - 8.0, y - 4.0, x1 + 8.0, y + 14.0),
        4.0,
        bar_bg,
    );
    draw::line(scene, t, Point::new(x0, y), Point::new(x1, y), 2.0, theme.text_primary);
    draw::line(scene, t, Point::new(x0, y - 3.0), Point::new(x0, y + 3.0), 2.0, theme.text_primary);
    draw::line(scene, t, Point::new(x1, y - 3.0), Point::new(x1, y + 3.0), 2.0, theme.text_primary);
    draw::text_center(scene, tr, t, &label, x0, x1, y + 2.0, 10.0, theme.text_dim);

    // Attribution — right-aligned, just above the scale bar. Anchoring it to
    // the right edge keeps it clear of the left-hand panel/drawer (which would
    // otherwise cover a bottom-left credit); the map data + basemap credits
    // must stay visible. Data is OpenStreetMap; the label-free raster style is
    // served by CARTO, so both are credited.
    let attr = "© OpenStreetMap contributors © CARTO";
    let aw = draw::measure_w(tr, attr, 10.0) + 14.0;
    let ax1 = clip.x1 - 24.0;
    let ax = ax1 - aw;
    let ay = clip.y1 - 46.0;
    draw::fill_rrect(
        scene,
        t,
        Rect::new(ax, ay, ax1, ay + 17.0),
        4.0,
        bar_bg,
    );
    draw::text(scene, tr, t, attr, ax + 7.0, ay + 3.0, 10.0, theme.text_dim);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{LatLon, MapModel, Poi};
    use crate::tiles::TileLayer;

    fn poi_at(id: &str, cat: Category, lat: f64, lon: f64) -> Poi {
        Poi {
            id: id.to_string(),
            name: id.to_string(),
            category: cat,
            pos: LatLon::new(lat, lon),
            address: String::new(),
            opening_hours: None,
            website: None,
            phone: None,
            kind: None,
        }
    }

    /// The declutter pass thins markers that would pile up within `min_px`,
    /// keeps the more salient category when two collide, and always keeps the
    /// selected POI — the same set the draw pass and the hit-test both consume.
    #[test]
    fn declutter_thins_overlapping_keeps_priority_and_selected() {
        let center = LatLon::new(48.1371, 11.5754);
        let proj = Projection::new(center, 17.0, Point::new(640.0, 400.0));

        // Two POIs at essentially the same coordinate (they project within a
        // pixel of each other) — a Shop and a Transit stop — plus one clearly
        // separate Cafe far to the east.
        let far = LatLon::new(center.lat, center.lon + 0.01);
        let pois = vec![
            poi_at("shop", Category::Shop, center.lat, center.lon),
            poi_at("transit", Category::Transit, center.lat, center.lon + 1e-6),
            poi_at("cafe-far", Category::Cafe, far.lat, far.lon),
        ];

        // No selection: the overlapping pair collapses to the higher-priority
        // Transit stop; the far Cafe always survives.
        let kept = declutter_indices(&proj, &pois, 24.0, None);
        let ids: Vec<&str> = kept.iter().map(|&i| pois[i].id.as_str()).collect();
        assert!(ids.contains(&"transit"), "salient category wins the contested cell: {ids:?}");
        assert!(!ids.contains(&"shop"), "the overlapped lower-priority marker is dropped: {ids:?}");
        assert!(ids.contains(&"cafe-far"), "a well-separated marker is always kept: {ids:?}");

        // Returned indices stay in input order (stable draw order).
        let mut sorted = kept.clone();
        sorted.sort_unstable();
        assert_eq!(kept, sorted, "kept indices preserve input order");

        // Selecting the otherwise-dropped Shop forces it to be kept (its info
        // card is open, so its marker must be on screen and clickable).
        let kept_sel = declutter_indices(&proj, &pois, 24.0, Some("shop"));
        let ids_sel: Vec<&str> = kept_sel.iter().map(|&i| pois[i].id.as_str()).collect();
        assert!(ids_sel.contains(&"shop"), "the selected POI is always kept: {ids_sel:?}");
    }

    /// The default map cartography reads as a light map even under the dark OS
    /// theme (the theme the shots and the default live session use). This is the
    /// fix for the "map too dark" murk: the map is its own light surface, not
    /// tinted down by the dark chrome. A genuinely dark map would be a separate
    /// cartography style, never forced onto the map by a dark chrome theme.
    #[test]
    fn default_map_palette_is_light() {
        let lum = |c: Color| {
            let c = c.to_rgba8();
            0.2126 * c.r as f64 + 0.7152 * c.g as f64 + 0.0722 * c.b as f64
        };
        let dark = MapPalette::from_theme(&Theme::dark());
        assert!(lum(dark.land) > 220.0, "land near-white under a dark theme: {}", lum(dark.land));
        assert!(lum(dark.road_res) > 220.0, "residential roads read light");
        assert!(lum(dark.water) > 170.0, "water is a light tint, not a dark slab");
        // Light theme stays light too.
        let light = MapPalette::from_theme(&Theme::light());
        assert!(lum(light.land) > 220.0, "land near-white under a light theme too");
    }

    /// The pre-tiles loading substrate is a calm DARK neutral surface under the
    /// dark theme — NOT the near-white cartographic land — so a first-run map
    /// never flashes bright before imagery arrives. It must read clearly darker
    /// than the (still-light) tile land underlay.
    #[test]
    fn loading_substrate_is_dark_and_not_white() {
        let lum = |c: Color| {
            let c = c.to_rgba8();
            0.2126 * c.r as f64 + 0.7152 * c.g as f64 + 0.0722 * c.b as f64
        };
        let dark = MapPalette::from_theme(&Theme::dark());
        assert!(
            lum(dark.substrate) < 90.0,
            "loading substrate is a dark surface, not a bright flash: {}",
            lum(dark.substrate)
        );
        assert!(
            lum(dark.substrate) + 100.0 < lum(dark.land),
            "substrate ({}) is much darker than the tile land underlay ({})",
            lum(dark.substrate),
            lum(dark.land)
        );
    }

    /// `draw_tiles` must NOT fetch or decode synchronously: when given an
    /// offline layer (no worker, empty cache), it draws placeholders, reports
    /// "no exact tiles", and leaves the resident set empty — proving the draw
    /// pass never blocks on the (slow) fetch/decode path.
    #[test]
    fn draw_tiles_does_not_fetch_synchronously() {
        let theme = Theme::dark();
        let pal = MapPalette::from_theme(&theme);
        let mut layer = TileLayer::offline();
        let model = MapModel::default();
        let clip = Rect::new(0.0, 0.0, 1280.0, 800.0);
        let view_center = clip.center();

        let mut scene = Scene::new();
        let t0 = std::time::Instant::now();
        let painted_exact =
            draw_tiles(&mut scene, Affine::IDENTITY, clip, &mut layer, &model, view_center, &pal);

        // Fast (no I/O), no real tile painted, and crucially nothing was
        // synchronously fetched/decoded into the resident store.
        assert!(t0.elapsed() < std::time::Duration::from_millis(50));
        assert!(!painted_exact);
        assert_eq!(layer.mem_len(), 0, "draw must not decode tiles inline");
        // The offline layer must not even mark tiles in-flight.
        assert!(!layer.has_inflight());
    }

    /// Zoom must never blank: with only z14 imagery cached, a zoom-*in* (to z15)
    /// reuses the z14 tiles as scaled *ancestors*, and a zoom-*out* (to z13)
    /// reuses them as *descendants* — both keep painting real imagery so the
    /// caller never covers the viewport with the reference-grid substrate.
    /// Guards the mid-zoom blank where `draw_tiles` reported "no imagery" during
    /// a transition and the substrate was painted straight over the placeholders.
    #[test]
    fn zoom_reuses_cached_tiles_and_never_blanks() {
        let theme = Theme::dark();
        let pal = MapPalette::from_theme(&theme);
        let clip = Rect::new(0.0, 0.0, 1280.0, 800.0);
        let vc = clip.center();
        let center = crate::model::LatLon::new(48.1371, 11.5754);

        // Seed the z14 tiles covering the viewport — the resident set you'd have
        // after sitting at z14 for a moment.
        let mut layer = TileLayer::offline();
        for (id, _) in TileLayer::visible_tiles(center, 14.0, vc, clip) {
            layer.insert_solid_tile(id);
        }
        assert!(layer.mem_len() > 0, "seeded z14 imagery");

        let paints = |layer: &mut TileLayer, zoom: f64| {
            let mut model = MapModel::default();
            model.center = center;
            model.zoom = zoom;
            let mut scene = Scene::new();
            draw_tiles(&mut scene, Affine::IDENTITY, clip, layer, &model, vc, &pal)
        };

        assert!(paints(&mut layer, 14.0), "exact-zoom tiles paint imagery");
        assert!(
            paints(&mut layer, 15.0),
            "zoom-in reuses z14 as scaled ancestors — must not blank"
        );
        assert!(
            paints(&mut layer, 13.0),
            "zoom-out reuses z14 as descendants — must not blank"
        );

        // A genuinely empty cache paints no imagery, so the caller still falls
        // back to the substrate/vector basemap on a true first frame.
        let mut empty = TileLayer::offline();
        assert!(!paints(&mut empty, 14.0), "empty cache paints no imagery");
    }

    /// A transit leg is drawn in its authentic GTFS `routeColor` when the feed
    /// provides one, and falls back to the theme per-mode colour otherwise.
    #[test]
    fn leg_color_uses_feed_color_then_theme_fallback() {
        use crate::model::TransitLeg;
        let theme = Theme::dark();
        let base = TransitLeg {
            mode: TransitMode::Subway,
            line: "U3".into(),
            headsign: String::new(),
            from_name: String::new(),
            to_name: String::new(),
            dep: String::new(),
            arr: String::new(),
            duration_min: 5,
            num_stops: 2,
            geometry: vec![crate::model::LatLon::new(0.0, 0.0), crate::model::LatLon::new(0.1, 0.1)],
            color: Some("eb690b".into()),
            text_color: Some("ffffff".into()),
            agency: "MVG".into(),
            realtime: true,
        };
        // Feed colour wins.
        assert_eq!(leg_color(&theme, &base), Color::from_rgba8(0xeb, 0x69, 0x0b, 255));
        // No feed colour → theme per-mode fallback (not the feed hex).
        let no_color = TransitLeg { color: None, ..base.clone() };
        assert_eq!(leg_color(&theme, &no_color), transit_leg_color(&theme, TransitMode::Subway));
        // A malformed hex also falls back rather than panicking.
        let bad = TransitLeg { color: Some("nothex".into()), ..base };
        assert_eq!(leg_color(&theme, &bad), transit_leg_color(&theme, TransitMode::Subway));
    }
}
