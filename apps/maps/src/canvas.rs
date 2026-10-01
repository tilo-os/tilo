//! The map layer: projects and paints the stylised vector basemap, the
//! active route polyline, and the place markers into a Vello scene.
//!
//! Colours derive from the `Theme` via [`MapPalette`] so the cartography
//! tracks the OS theme (dark map under a dark theme, etc.) instead of
//! hard-coding a palette.

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{BlendMode, Color};

use ui::paint::PaintCtx;
use ui::text_render::{TextRenderer, TextStyle, TextWeight};
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
    /// Resolve the cartographic palette from the active theme. The map tracks
    /// the OS theme — a light map under a light theme, a dark map under a dark
    /// theme — so the vector fallback, the tile land underlay and the loading
    /// substrate all share the basemap's tone and a view is never a mix of light
    /// and dark tiles (the reported light-centre/dark-edge bug). The style
    /// decision is the *same* one the raster layer uses
    /// ([`crate::tiles::TileStyle::from_theme`]), so the drawn colours and the
    /// tone the tiles are decoded in always agree.
    pub fn from_theme(theme: &Theme) -> Self {
        use crate::tiles::TileStyle;
        if TileStyle::from_theme(theme) == TileStyle::Dark {
            Self::dark(theme)
        } else {
            Self::light(theme)
        }
    }

    /// The light cartography: a warm near-white paper land, matching the OSM
    /// Standard raster it underlays. Hues track the theme (water = info,
    /// parks = success); the route/markers track the accent.
    fn light(theme: &Theme) -> Self {
        let sem = &theme.tokens.colors.semantic;
        let w = Color::WHITE;
        let k = Color::BLACK;
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
            // The loading substrate shares the land tone (a faint step darker for
            // the reference grid) so a first-run/mid-load view never seams.
            substrate: draw::mix(land, k, 0.05),
            substrate_grid: draw::mix(land, k, 0.14),
            traffic_free: sem.success,
            traffic_moderate: sem.warning,
            traffic_heavy: draw::mix(sem.warning, sem.danger, 0.55),
            traffic_severe: sem.danger,
            traffic_casing: draw::mix(land, k, 0.42),
        }
    }

    /// The dark cartography: a deep neutral land matching the dark re-tone of
    /// the OSM raster, with light-grey road ribbons and dark-tinted
    /// water/parks so the map reads coherently dark — not a light map dimmed.
    fn dark(theme: &Theme) -> Self {
        let sem = &theme.tokens.colors.semantic;
        let w = Color::WHITE;
        let k = Color::BLACK;
        // Deep land, nudged off the chrome surface toward black so the basemap is
        // its own quiet ground beneath the chrome.
        let land = draw::mix(theme.tokens.colors.surfaces.base, k, 0.28);
        Self {
            land,
            building: draw::mix(land, w, 0.07),
            building_edge: draw::mix(land, w, 0.14),
            park: draw::mix(land, sem.success, 0.22),
            park_edge: draw::mix(land, sem.success, 0.36),
            water: draw::mix(land, sem.info, 0.30),
            // Roads read as light ribbons on the dark land.
            road_res: draw::mix(land, w, 0.22),
            road_res_casing: draw::mix(land, k, 0.5),
            road_secondary: draw::mix(land, sem.warning, 0.34),
            road_primary: draw::mix(land, sem.warning, 0.52),
            road_label: draw::mix(land, w, 0.72),
            route: theme.accent_primary,
            // A darker casing under the accent route so it reads on dark land.
            route_casing: draw::mix(theme.accent_primary, k, 0.45),
            // A near-white ring keeps pins crisp on the dark basemap.
            marker_ring: draw::mix(land, w, 0.9),
            // Substrate shares the land tone, one faint step darker for the grid.
            substrate: draw::mix(land, k, 0.18),
            substrate_grid: draw::mix(land, w, 0.10),
            traffic_free: sem.success,
            traffic_moderate: sem.warning,
            traffic_heavy: draw::mix(sem.warning, sem.danger, 0.55),
            traffic_severe: sem.danger,
            // A light casing so the signal ribbons separate on the dark land.
            traffic_casing: draw::mix(land, w, 0.22),
        }
    }
}

/// The signal colour for a congestion level, resolved from the palette.
pub fn traffic_color(pal: &MapPalette, level: crate::traffic::Congestion) -> Color {
    use crate::traffic::Congestion::{Free, Heavy, Moderate, Severe};
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
        Category::Landmark | Category::Hospital => c.semantic.danger,
        Category::Cafe | Category::Shop => c.semantic.warning,
        Category::Restaurant => draw::mix(c.semantic.danger, c.semantic.warning, 0.5),
        Category::Bar => c.accent[6],
        Category::Museum => c.semantic.info,
        Category::Park => c.semantic.success,
        Category::Transit => c.accent[ui::tokens::colors::ACCENT_PRIMARY_IDX],
        Category::Hotel => c.accent[7],
        Category::Other => c.neutral[6],
    }
}

/// Marker draw priority for decluttering — a more salient category wins a
/// contested screen cell, so a dense area keeps its landmarks / transit stops
/// rather than a random scatter of shops.
pub fn category_priority(cat: Category) -> u8 {
    match cat {
        Category::Transit | Category::Landmark => 6,
        Category::Hospital | Category::Museum => 5,
        Category::Park => 4,
        Category::Hotel | Category::Restaurant => 3,
        Category::Cafe | Category::Bar => 2,
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
        let sa = pois.get(a).is_some_and(|p| selected == Some(p.id.as_str()));
        let sb = pois.get(b).is_some_and(|p| selected == Some(p.id.as_str()));
        let pa = pois.get(a).map_or(0, |p| category_priority(p.category));
        let pb = pois.get(b).map_or(0, |p| category_priority(p.category));
        sb.cmp(&sa).then(pb.cmp(&pa)).then(a.cmp(&b))
    });
    let min2 = (min_px * min_px).max(0.0);
    let mut kept_pts: Vec<Point> = Vec::new();
    let mut keep = vec![false; pois.len()];
    for &i in &order {
        let Some(&p) = pts.get(i) else { continue };
        let clear = kept_pts.iter().all(|k| {
            let dx = k.x - p.x;
            let dy = k.y - p.y;
            dx * dx + dy * dy >= min2
        });
        if clear {
            if let Some(k) = keep.get_mut(i) {
                *k = true;
            }
            kept_pts.push(p);
        }
    }
    (0..pois.len())
        .filter(|&i| keep.get(i).copied().unwrap_or(false))
        .collect()
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
    scene.push_layer(
        vello::peniko::Fill::NonZero,
        BlendMode::default(),
        1.0,
        t,
        &clip,
    );

    let mut painted_any = false;
    let mut pending: Vec<crate::tiles::TileId> = Vec::new();

    for (id, rect) in &visible {
        // (1) land underlay.
        tiles::paint_placeholder(scene, t, *rect, pal.land);

        let have_exact = tiles_layer.get(*id).is_some();
        if !have_exact {
            // (2) ancestor placeholder (blurry lower-z fill for zoom-in / pan).
            if let Some((anc_id, levels)) = tiles_layer.ancestor_placeholder(*id)
                && let Some(anc_img) = tiles_layer.get(anc_id).cloned()
            {
                tiles::paint_ancestor(scene, t, &anc_img, anc_id, *id, levels, *rect);
                painted_any = true;
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
    scene.push_layer(
        vello::peniko::Fill::NonZero,
        BlendMode::default(),
        1.0,
        t,
        &clip,
    );
    for (_, rect) in &tiles {
        // Quarter-tile minor grid for a paper-like texture.
        for k in 1..4 {
            let fx = rect.x0 + rect.width() * f64::from(k) / 4.0;
            let fy = rect.y0 + rect.height() * f64::from(k) / 4.0;
            draw::line(
                scene,
                t,
                Point::new(fx, rect.y0),
                Point::new(fx, rect.y1),
                0.5,
                minor,
            );
            draw::line(
                scene,
                t,
                Point::new(rect.x0, fy),
                Point::new(rect.x1, fy),
                0.5,
                minor,
            );
        }
        // Major tile boundary (top + left edge; neighbours cover the rest).
        draw::line(
            scene,
            t,
            Point::new(rect.x0, rect.y0),
            Point::new(rect.x0, rect.y1),
            1.0,
            major,
        );
        draw::line(
            scene,
            t,
            Point::new(rect.x0, rect.y0),
            Point::new(rect.x1, rect.y0),
            1.0,
            major,
        );
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
    draw::polyline(
        scene,
        t,
        &river_pts,
        road_width(RoadClass::Primary, proj.zoom()) * 3.2,
        pal.water,
    );

    for a in base.areas.iter().filter(|a| a.kind == AreaKind::Building) {
        draw_area(scene, t, proj, a, pal.building, Some(pal.building_edge));
    }

    // Roads: casings first (so intersections look layered), then surfaces.
    for r in &base.roads {
        if r.class == RoadClass::Residential {
            continue;
        }
        let pts = project_pts(proj, &r.pts);
        draw::polyline(
            scene,
            t,
            &pts,
            road_width(r.class, proj.zoom()) + 2.0,
            pal.road_res_casing,
        );
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

fn draw_area(
    scene: &mut Scene,
    t: Affine,
    proj: &Projection,
    a: &Area,
    fill: Color,
    edge: Option<Color>,
) {
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
    let (Some(&start), Some(&end)) = (pts.first(), pts.last()) else {
        return;
    };
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
        draw::polyline(
            scene,
            t,
            &pts,
            width(seg.level),
            traffic_color(pal, seg.level),
        );
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
    scene.push_layer(
        vello::peniko::Fill::NonZero,
        BlendMode::default(),
        1.0,
        t,
        &clip,
    );
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
#[allow(
    clippy::many_single_char_names,
    reason = "conventional short names for a 2D segment (a, b), its direction (dx, dy) and \
              the running dash offset (d) — the standard vocabulary for this kind of geometry"
)]
fn draw_dashed(
    scene: &mut Scene,
    t: Affine,
    pts: &[Point],
    width: f64,
    color: Color,
    dash: f64,
    gap: f64,
) {
    if pts.len() < 2 {
        return;
    }
    let period = dash + gap;
    if period <= 0.0 {
        return;
    }
    for seg in pts.windows(2) {
        let &[a, b] = seg else { continue };
        let len = a.distance(b);
        if len < 1e-3 {
            continue;
        }
        let (dx, dy) = ((b.x - a.x) / len, (b.y - a.y) / len);
        // Bounded by the segment length / period rather than a float `while`,
        // so a pathological input can't spin forever on rounding.
        let steps = (len / period).ceil() as u32 + 1;
        for k in 0..steps {
            let d = period * f64::from(k);
            if d >= len {
                break;
            }
            let s = Point::new(a.x + dx * d, a.y + dy * d);
            let e_len = (d + dash).min(len);
            let e = Point::new(a.x + dx * e_len, a.y + dy * e_len);
            draw::line(scene, t, s, e, width, color);
        }
    }
}

/// What a map overlay layer paints against: the viewport projection, the
/// themed map palette, and the visible map area it stays inside.
#[derive(Clone, Copy)]
pub struct MapView<'a> {
    pub proj: &'a Projection,
    pub pal: &'a MapPalette,
    pub clip: Rect,
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
    scene.push_layer(
        vello::peniko::Fill::NonZero,
        BlendMode::default(),
        1.0,
        t,
        &clip,
    );
    // Origin marker, if a route/origin is set.
    if let Some(o) = model.origin {
        let p = proj.project(o);
        if clip.contains(p) {
            draw::fill_circle(
                scene,
                t,
                Point::new(p.x, p.y + 1.0),
                6.5,
                theme.tokens.elevation.e2.color,
            );
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
        draw_pin(
            scene,
            t,
            p,
            category_color(theme, place.category),
            pal,
            theme,
            false,
        );
    }

    if let Some(sel) = model.selected_place() {
        let p = proj.project(sel.pos);
        if clip.contains(p) {
            draw_pin(
                scene,
                t,
                p,
                category_color(theme, sel.category),
                pal,
                theme,
                true,
            );
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
                    theme,
                    rect,
                    6.0,
                    theme.tokens.colors.surfaces.raised,
                );
                PaintCtx::new(scene, t, tr, theme).text(
                    label,
                    &TextStyle::new(12.0).with_weight(TextWeight::Semibold),
                    theme.text_primary,
                    Point::new(lx + pad, ly + 4.0),
                    None,
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
pub fn draw_pois(
    ctx: &mut PaintCtx<'_>,
    view: MapView<'_>,
    pois: &[Poi],
    visible: &[usize],
    selected: Option<&str>,
) {
    let MapView { proj, clip, .. } = view;
    ctx.scene.push_layer(
        vello::peniko::Fill::NonZero,
        BlendMode::default(),
        1.0,
        ctx.transform,
        &clip,
    );
    for &i in visible {
        let Some(poi) = pois.get(i) else {
            continue;
        };
        if selected == Some(poi.id.as_str()) {
            continue; // draw the selected POI last, on top
        }
        if !clip.contains(proj.project(poi.pos)) {
            continue;
        }
        draw_poi_dot(ctx, view, poi, false);
    }
    if let Some(sel) = selected
        && let Some(poi) = pois.iter().find(|p| p.id == sel)
        && clip.contains(proj.project(poi.pos))
    {
        draw_poi_dot(ctx, view, poi, true);
    }
    ctx.scene.pop_layer();
}

/// One POI marker at its projected position: a category dot, or past
/// [`POI_ICON_ZOOM`] a pictogram disc, with a name plate when zoomed in far
/// enough or `selected`.
fn draw_poi_dot(ctx: &mut PaintCtx<'_>, view: MapView<'_>, poi: &Poi, selected: bool) {
    let MapView { proj, pal, clip } = view;
    let (scene, t, theme) = (&mut *ctx.scene, ctx.transform, ctx.theme);
    let p = proj.project(poi.pos);
    let icons = proj.zoom() >= POI_ICON_ZOOM;
    let label = selected || proj.zoom() >= 16.5;
    let col = category_color(theme, poi.category);
    let r = match (icons, selected) {
        (true, true) => 13.0,
        (true, false) => 9.0,
        (false, true) => 8.5,
        (false, false) => 4.5,
    };
    draw::fill_circle(
        scene,
        t,
        Point::new(p.x, p.y + 0.9),
        r + 1.3,
        theme.tokens.elevation.e2.color,
    );
    draw::fill_circle(scene, t, p, r, col);
    draw::stroke_circle(
        scene,
        t,
        p,
        r,
        if selected { 2.5 } else { 1.6 },
        pal.marker_ring,
    );
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
        let tw = draw::measure_w(ctx.text, &poi.name, size);
        let lx = p.x + r + 6.0;
        let ly = p.y - f64::from(size) * 0.72;
        let plate = Rect::new(
            lx - 4.0,
            ly - 1.0,
            lx + tw + 6.0,
            ly + f64::from(size) + 3.0,
        );
        // Only draw the name plate when it fits in the map area, so it is never
        // sliced at the drawer / screen edge into an unreadable fragment. The
        // marker still shows; the info card / list already carries the name.
        if plate.x0 >= clip.x0 && plate.x1 <= clip.x1 && plate.y0 >= clip.y0 {
            draw::fill_round(
                ctx.scene,
                t,
                plate,
                4.0,
                draw::alpha(theme.tokens.colors.surfaces.base, 0.74),
            );
            ctx.text(
                &poi.name,
                &TextStyle::new(size).with_weight(TextWeight::Medium),
                theme.text_primary,
                Point::new(lx, ly),
                None,
            );
        }
    }
}

/// Draw a compact white category pictogram centred at `c` on a disc of radius
/// `r`. `fg` is the icon colour (the near-white marker ring); `bg` is the disc
/// colour, used for cut-outs (e.g. train windows) so they read against the fill.
/// Deliberately built from a handful of primitives — no icon-font dependency, so
/// it renders identically headless and live and tracks the OS theme.
#[allow(
    clippy::too_many_lines,
    reason = "one match arm per Category variant, each a short, self-contained icon built \
              from a handful of primitives; splitting per-arm would only relocate the code"
)]
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
            draw::fill_round(
                scene,
                t,
                Rect::new(c.x - s, c.y - s * 0.32, c.x + s, c.y + s * 0.32),
                1.5,
                fg,
            );
            draw::fill_round(
                scene,
                t,
                Rect::new(c.x - s * 0.32, c.y - s, c.x + s * 0.32, c.y + s),
                1.5,
                fg,
            );
        }
        Category::Landmark => {
            let mut pts = Vec::with_capacity(10);
            for k in 0..10 {
                let ang = -std::f64::consts::FRAC_PI_2 + f64::from(k) * std::f64::consts::PI / 5.0;
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
            draw::fill_round(
                scene,
                t,
                Rect::new(
                    c.x - s * 0.6,
                    c.y - s * 0.35,
                    c.x + s * 0.35,
                    c.y + s * 0.45,
                ),
                2.0,
                fg,
            );
            draw::stroke_circle(
                scene,
                t,
                Point::new(c.x + s * 0.5, c.y + s * 0.02),
                s * 0.3,
                lw,
                fg,
            );
            ln(
                scene,
                c.x - s * 0.8,
                c.y + s * 0.68,
                c.x + s * 0.6,
                c.y + s * 0.68,
                lw,
            );
        }
        Category::Restaurant => {
            // Fork (left): three short prongs onto a stem.
            for dx in [-0.68, -0.45, -0.22] {
                ln(
                    scene,
                    c.x + dx * s,
                    c.y - s * 0.7,
                    c.x + dx * s,
                    c.y - s * 0.18,
                    lw,
                );
            }
            ln(
                scene,
                c.x - s * 0.45,
                c.y - s * 0.18,
                c.x - s * 0.45,
                c.y + s * 0.82,
                lw * 1.5,
            );
            // Knife (right): a single tapering blade+handle.
            ln(
                scene,
                c.x + s * 0.5,
                c.y - s * 0.7,
                c.x + s * 0.5,
                c.y + s * 0.82,
                lw * 1.7,
            );
        }
        Category::Bar => {
            let bowl = [
                Point::new(c.x - s * 0.72, c.y - s * 0.55),
                Point::new(c.x + s * 0.72, c.y - s * 0.55),
                Point::new(c.x, c.y + s * 0.18),
            ];
            draw::polygon(scene, t, &bowl, fg);
            ln(scene, c.x, c.y + s * 0.18, c.x, c.y + s * 0.72, lw);
            ln(
                scene,
                c.x - s * 0.4,
                c.y + s * 0.72,
                c.x + s * 0.4,
                c.y + s * 0.72,
                lw,
            );
        }
        Category::Museum => {
            let roof = [
                Point::new(c.x - s * 0.82, c.y - s * 0.18),
                Point::new(c.x + s * 0.82, c.y - s * 0.18),
                Point::new(c.x, c.y - s * 0.78),
            ];
            draw::polygon(scene, t, &roof, fg);
            for dx in [-0.45, 0.0, 0.45] {
                ln(
                    scene,
                    c.x + dx * s,
                    c.y - s * 0.12,
                    c.x + dx * s,
                    c.y + s * 0.55,
                    lw * 1.2,
                );
            }
            ln(
                scene,
                c.x - s * 0.78,
                c.y + s * 0.66,
                c.x + s * 0.78,
                c.y + s * 0.66,
                lw * 1.3,
            );
        }
        Category::Transit => {
            draw::fill_round(
                scene,
                t,
                Rect::new(c.x - s * 0.6, c.y - s * 0.72, c.x + s * 0.6, c.y + s * 0.5),
                2.5,
                fg,
            );
            // Two windows cut out of the body (disc colour).
            draw::fill_round(
                scene,
                t,
                Rect::new(
                    c.x - s * 0.42,
                    c.y - s * 0.5,
                    c.x - s * 0.06,
                    c.y - s * 0.06,
                ),
                1.5,
                bg,
            );
            draw::fill_round(
                scene,
                t,
                Rect::new(
                    c.x + s * 0.06,
                    c.y - s * 0.5,
                    c.x + s * 0.42,
                    c.y - s * 0.06,
                ),
                1.5,
                bg,
            );
            draw::fill_circle(
                scene,
                t,
                Point::new(c.x - s * 0.32, c.y + s * 0.62),
                s * 0.16,
                fg,
            );
            draw::fill_circle(
                scene,
                t,
                Point::new(c.x + s * 0.32, c.y + s * 0.62),
                s * 0.16,
                fg,
            );
        }
        Category::Hotel => {
            ln(
                scene,
                c.x - s * 0.72,
                c.y - s * 0.5,
                c.x - s * 0.72,
                c.y + s * 0.5,
                lw * 1.4,
            );
            draw::fill_round(
                scene,
                t,
                Rect::new(
                    c.x - s * 0.72,
                    c.y + s * 0.08,
                    c.x + s * 0.78,
                    c.y + s * 0.46,
                ),
                2.0,
                fg,
            );
            draw::fill_round(
                scene,
                t,
                Rect::new(c.x - s * 0.5, c.y - s * 0.2, c.x - s * 0.08, c.y + s * 0.12),
                2.0,
                fg,
            );
        }
        Category::Shop => {
            draw::stroke_circle(scene, t, Point::new(c.x, c.y - s * 0.22), s * 0.32, lw, fg);
            draw::fill_round(
                scene,
                t,
                Rect::new(c.x - s * 0.6, c.y - s * 0.18, c.x + s * 0.6, c.y + s * 0.72),
                2.5,
                fg,
            );
        }
        Category::Other => {
            draw::fill_circle(scene, t, c, s * 0.4, fg);
        }
    }
}

/// Build a crisp teardrop "map pin" path: a circular head of radius `head_r`
/// centred `height` above `anchor`, its two sides running tangent down to a
/// point exactly at `anchor`. One closed path (no circle-plus-triangle seam), so
/// the fill and its outline read clean at any size.
fn pin_path(anchor: Point, head_r: f64, height: f64) -> vello::kurbo::BezPath {
    use std::f64::consts::{FRAC_PI_2, PI};
    let cx = anchor.x;
    let cy = anchor.y - height;
    // Angle from the (downward) tip direction to each tangent point on the head.
    let alpha = (head_r / height).clamp(-1.0, 1.0).acos();
    let theta_r = FRAC_PI_2 - alpha; // lower-right tangent point
    let sweep = -(2.0 * PI - 2.0 * alpha); // the major arc, up and over the top
    let pt = |phi: f64| Point::new(cx + head_r * phi.cos(), cy + head_r * phi.sin());
    let mut p = vello::kurbo::BezPath::new();
    p.move_to(anchor);
    p.line_to(pt(theta_r));
    let n = 32;
    for i in 1..=n {
        p.line_to(pt(theta_r + sweep * (f64::from(i) / f64::from(n))));
    }
    p.line_to(anchor);
    p.close_path();
    p
}

/// Draw a themed teardrop pin anchored at `anchor`: a soft ground shadow, a
/// category-coloured drop with a crisp light outline (so it reads on any basemap
/// tone), and a light "eye". The selected pin is larger and wears a soft accent
/// halo. All colours come from the `Theme`-derived palette / category colour.
fn draw_pin(
    scene: &mut Scene,
    t: Affine,
    anchor: Point,
    color: Color,
    pal: &MapPalette,
    theme: &Theme,
    selected: bool,
) {
    let r = if selected { 11.0 } else { 8.0 };
    let h = if selected { 30.0 } else { 22.0 };
    let center = Point::new(anchor.x, anchor.y - h);

    // Soft ground shadow at the tip, so the pin stands on the map rather than
    // being pasted onto it.
    let shadow = vello::kurbo::Ellipse::new(anchor, (r * 0.62, r * 0.24), 0.0);
    scene.fill(
        vello::peniko::Fill::NonZero,
        t,
        theme.tokens.elevation.e2.color,
        None,
        &shadow,
    );

    // Selected: a soft accent halo behind the head.
    if selected {
        draw::fill_circle(scene, t, center, r + 6.0, draw::alpha(color, 0.20));
    }

    // The teardrop: stroke the light outline first, then fill the colour over
    // its inner half — leaving a clean light ring around the drop on any tone.
    let body = pin_path(anchor, r, h);
    draw::stroke_path(
        scene,
        t,
        &body,
        if selected { 3.0 } else { 2.4 },
        pal.marker_ring,
    );
    draw::fill_path(scene, t, &body, color);

    // The classic pin "eye": a light disc with a small tinted centre.
    draw::fill_circle(scene, t, center, r * 0.40, pal.marker_ring);
    draw::fill_circle(scene, t, center, r * 0.20, draw::alpha(color, 0.9));
}

/// Draw the bottom-right scale bar into `clip`, its right end at `right_x`
/// (kept left of the ⋯ overflow button). Returns the left edge of its backing
/// pill, where [`draw_attribution`] ends.
pub fn draw_scale_bar(
    scene: &mut Scene,
    t: Affine,
    tr: &mut TextRenderer,
    clip: Rect,
    right_x: f64,
    model: &MapModel,
    theme: &Theme,
) -> f64 {
    let (label, px) = geo::scale_bar(model.center, model.zoom, 110.0);
    let y = clip.y1 - 26.0;
    let x1 = right_x;
    let x0 = x1 - px;
    let bar_bg = draw::alpha(theme.tokens.colors.surfaces.base, 0.7);
    draw::fill_round(
        scene,
        t,
        Rect::new(x0 - 8.0, y - 4.0, x1 + 8.0, y + 14.0),
        4.0,
        bar_bg,
    );
    draw::line(
        scene,
        t,
        Point::new(x0, y),
        Point::new(x1, y),
        2.0,
        theme.text_primary,
    );
    draw::line(
        scene,
        t,
        Point::new(x0, y - 3.0),
        Point::new(x0, y + 3.0),
        2.0,
        theme.text_primary,
    );
    draw::line(
        scene,
        t,
        Point::new(x1, y - 3.0),
        Point::new(x1, y + 3.0),
        2.0,
        theme.text_primary,
    );
    draw::text_center(
        &mut PaintCtx::new(scene, t, tr, theme),
        &label,
        Point::new((x0 + x1) / 2.0, y + 2.0),
        10.0,
        theme.text_dim,
    );
    x0 - 8.0
}

/// Draw the tile licence credit on the map: left of the scale bar on its row
/// (the bar spans `scale_x0..scale_x1`), or right-aligned above the bar when
/// the map is too narrow for both. The OSM tile policy requires it visible on
/// the map itself, not behind a menu.
pub fn draw_attribution(
    scene: &mut Scene,
    t: Affine,
    tr: &mut TextRenderer,
    clip: Rect,
    scale_x0: f64,
    scale_x1: f64,
    theme: &Theme,
) {
    let size = theme.tokens.typography.scale.xs;
    let w = draw::measure_w(tr, crate::tiles::TILE_ATTRIBUTION, size);
    let row_y = clip.y1 - 26.0;
    let beside = scale_x0 - 14.0;
    let (x1, y) = if beside - w - 8.0 >= clip.x0 + 8.0 {
        (beside, row_y)
    } else {
        (scale_x1, row_y - 24.0)
    };
    let x0 = x1 - w;
    draw::fill_round(
        scene,
        t,
        Rect::new(x0 - 8.0, y - 4.0, x1 + 8.0, y + 14.0),
        4.0,
        draw::alpha(theme.tokens.colors.surfaces.base, 0.7),
    );
    draw::text_right(
        scene,
        tr,
        t,
        crate::tiles::TILE_ATTRIBUTION,
        x1,
        y + 1.0,
        size,
        theme.text_dim,
    );
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

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn map_palette_markers_and_legs_follow_theme_and_priority() {
        // Phase `declutter_thins_overlapping_keeps_priority_and_selected`.
        // The declutter pass thins markers that would pile up within `min_px`,
        // keeps the more salient category when two collide, and always keeps the
        // selected POI — the same set the draw pass and the hit-test both consume.
        {
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
            assert!(
                ids.contains(&"transit"),
                "salient category wins the contested cell: {ids:?}"
            );
            assert!(
                !ids.contains(&"shop"),
                "the overlapped lower-priority marker is dropped: {ids:?}"
            );
            assert!(
                ids.contains(&"cafe-far"),
                "a well-separated marker is always kept: {ids:?}"
            );

            // Returned indices stay in input order (stable draw order).
            let mut sorted = kept.clone();
            sorted.sort_unstable();
            assert_eq!(kept, sorted, "kept indices preserve input order");

            // Selecting the otherwise-dropped Shop forces it to be kept (its info
            // card is open, so its marker must be on screen and clickable).
            let kept_sel = declutter_indices(&proj, &pois, 24.0, Some("shop"));
            let ids_sel: Vec<&str> = kept_sel.iter().map(|&i| pois[i].id.as_str()).collect();
            assert!(
                ids_sel.contains(&"shop"),
                "the selected POI is always kept: {ids_sel:?}"
            );
        }

        // Phase `map_palette_follows_theme`.
        // The cartography follows the theme: a light map under a light theme, a
        // dark map under a dark theme — never a light map forced under dark chrome
        // (which read as a mixed light/dark view). This is the fix for the tile-tone
        // bug: the drawn colours agree with the tone the raster layer decodes in.
        {
            let lum = |c: Color| {
                let c = c.to_rgba8();
                0.2126 * f64::from(c.r) + 0.7152 * f64::from(c.g) + 0.0722 * f64::from(c.b)
            };
            let dark = MapPalette::from_theme(&Theme::dark());
            assert!(
                lum(dark.land) < 70.0,
                "dark theme => a dark basemap land, got {}",
                lum(dark.land)
            );
            let light = MapPalette::from_theme(&Theme::light());
            assert!(
                lum(light.land) > 220.0,
                "light theme => near-white land, got {}",
                lum(light.land)
            );
            // Roads stay legible against the land in either theme (real contrast):
            // dark uses light ribbons on dark land; light uses coloured arterials.
            assert!(
                (lum(dark.road_res) - lum(dark.land)).abs() > 30.0,
                "dark residential roads must read against the dark land"
            );
            assert!(
                (lum(light.road_primary) - lum(light.land)).abs() > 15.0,
                "light arterials must read against the paper land"
            );
        }

        // Phase `loading_substrate_matches_land_tone`.
        // The pre-tiles loading substrate shares the land tone in *each* theme, so a
        // first-run or mid-load view is never a light centre with dark edges (or the
        // reverse) — the exact seam the user reported. It differs from the land only
        // faintly (for the reference grid), never by a light/dark jump.
        {
            let lum = |c: Color| {
                let c = c.to_rgba8();
                0.2126 * f64::from(c.r) + 0.7152 * f64::from(c.g) + 0.0722 * f64::from(c.b)
            };
            for theme in [Theme::dark(), Theme::light()] {
                let p = MapPalette::from_theme(&theme);
                assert!(
                    (lum(p.substrate) - lum(p.land)).abs() < 45.0,
                    "substrate ({}) must share the land tone ({}) — no light/dark seam",
                    lum(p.substrate),
                    lum(p.land)
                );
            }
        }

        // Phase `pin_path_is_anchored_and_encloses_head`.
        // The redesigned pin is a single closed teardrop whose tip sits exactly on
        // the anchor and whose head is a circle of radius `r` centred `h` above it —
        // so it points at the right coordinate and encloses a proper head (not a
        // disjoint circle + triangle).
        {
            use vello::kurbo::Shape as _;
            let anchor = Point::new(100.0, 200.0);
            let (r, h) = (8.0, 22.0);
            let bb = pin_path(anchor, r, h).bounding_box();
            // The tip is the lowest point, at the anchor.
            assert!(
                (bb.y1 - anchor.y).abs() < 0.5,
                "pin tip on the anchor, got y1={}",
                bb.y1
            );
            // The head reaches up to about anchor.y - h - r.
            assert!(
                (bb.y0 - (anchor.y - h - r)).abs() < 1.5,
                "head top near anchor.y-h-r, got y0={}",
                bb.y0
            );
            // Width is about the head diameter, centred on the anchor x.
            assert!(
                (bb.width() - 2.0 * r).abs() < 2.0,
                "width ~ head diameter, got {}",
                bb.width()
            );
            assert!(
                (0.5 * (bb.x0 + bb.x1) - anchor.x).abs() < 0.5,
                "the pin is centred on the anchor x"
            );
        }

        // Phase `leg_color_uses_feed_color_then_theme_fallback`.
        // A transit leg is drawn in its authentic GTFS `routeColor` when the feed
        // provides one, and falls back to the theme per-mode colour otherwise.
        {
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
                geometry: vec![
                    crate::model::LatLon::new(0.0, 0.0),
                    crate::model::LatLon::new(0.1, 0.1),
                ],
                color: Some("eb690b".into()),
                text_color: Some("ffffff".into()),
                agency: "MVG".into(),
                realtime: true,
            };
            // Feed colour wins.
            assert_eq!(
                leg_color(&theme, &base),
                Color::from_rgba8(0xeb, 0x69, 0x0b, 255)
            );
            // No feed colour → theme per-mode fallback (not the feed hex).
            let no_color = TransitLeg {
                color: None,
                ..base.clone()
            };
            assert_eq!(
                leg_color(&theme, &no_color),
                transit_leg_color(&theme, TransitMode::Subway)
            );
            // A malformed hex also falls back rather than panicking.
            let bad = TransitLeg {
                color: Some("nothex".into()),
                ..base
            };
            assert_eq!(
                leg_color(&theme, &bad),
                transit_leg_color(&theme, TransitMode::Subway)
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn drawing_markers_and_tiles_never_fetches_synchronously_or_blanks() {
        // Phase `selected_marker_renders_via_draw_markers`.
        // The selected place marker renders through the real `draw_markers` path
        // (teardrop pin + halo + floating label) without panicking, under the theme
        // palette — the production render path the shots exercise.
        {
            use crate::model::Place;
            let theme = Theme::dark();
            let pal = MapPalette::from_theme(&theme);
            let mut tr = TextRenderer::new();
            let center = LatLon::new(48.14, 11.58);
            let model = MapModel {
                center,
                zoom: 16.0,
                places: vec![Place::new(
                    "a",
                    "Alpha",
                    Category::Cafe,
                    "1 Main St",
                    center,
                )],
                selected: Some("a".to_string()),
                ..Default::default()
            };
            let proj = Projection::new(model.center, model.zoom, Point::new(640.0, 400.0));
            let clip = Rect::new(0.0, 0.0, 1280.0, 800.0);
            let mut scene = Scene::new();
            draw_markers(
                &mut scene,
                Affine::IDENTITY,
                &mut tr,
                &proj,
                &model,
                &theme,
                &pal,
                clip,
            );
            // The selected place projects into the clip, so the pin path was emitted.
            assert!(
                clip.contains(proj.project(model.center)),
                "the marker is on screen"
            );
        }

        // Phase `draw_tiles_does_not_fetch_synchronously`.
        // `draw_tiles` must NOT fetch or decode synchronously: when given an
        // offline layer (no worker, empty cache), it draws placeholders, reports
        // "no exact tiles", and leaves the resident set empty — proving the draw
        // pass never blocks on the (slow) fetch/decode path.
        {
            let theme = Theme::dark();
            let pal = MapPalette::from_theme(&theme);
            let mut layer = TileLayer::offline();
            let model = MapModel::default();
            let clip = Rect::new(0.0, 0.0, 1280.0, 800.0);
            let view_center = clip.center();

            let mut scene = Scene::new();
            let t0 = ui::time::now();
            let painted_exact = draw_tiles(
                &mut scene,
                Affine::IDENTITY,
                clip,
                &mut layer,
                &model,
                view_center,
                &pal,
            );

            // Fast (no I/O), no real tile painted, and crucially nothing was
            // synchronously fetched/decoded into the resident store.
            assert!(t0.elapsed() < std::time::Duration::from_millis(50));
            assert!(!painted_exact);
            assert_eq!(layer.mem_len(), 0, "draw must not decode tiles inline");
            // The offline layer must not even mark tiles in-flight.
            assert!(!layer.has_inflight());
        }

        // Phase `zoom_reuses_cached_tiles_and_never_blanks`.
        // Zoom must never blank: with only z14 imagery cached, a zoom-*in* (to z15)
        // reuses the z14 tiles as scaled *ancestors*, and a zoom-*out* (to z13)
        // reuses them as *descendants* — both keep painting real imagery so the
        // caller never covers the viewport with the reference-grid substrate.
        // Guards the mid-zoom blank where `draw_tiles` reported "no imagery" during
        // a transition and the substrate was painted straight over the placeholders.
        {
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
                let model = MapModel {
                    center,
                    zoom,
                    ..Default::default()
                };
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
    }
}
