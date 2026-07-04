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
use crate::model::{Category, MapModel};
use crate::sample::{Area, AreaKind, Basemap, RoadClass};
use crate::tiles::{self, TileLayer};

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
}

impl MapPalette {
    pub fn from_theme(theme: &Theme) -> Self {
        let n = &theme.tokens.colors.neutral;
        let sem = &theme.tokens.colors.semantic;
        let base = theme.tokens.colors.surfaces.base;
        Self {
            land: base,
            building: draw::mix(n[2], base, 0.35),
            building_edge: draw::mix(n[3], base, 0.4),
            park: draw::mix(sem.success, base, 0.66),
            park_edge: draw::mix(sem.success, base, 0.5),
            water: draw::mix(sem.info, base, 0.55),
            road_res: draw::mix(n[4], base, 0.25),
            road_res_casing: draw::mix(n[1], base, 0.2),
            road_secondary: draw::mix(n[6], base, 0.15),
            road_primary: draw::mix(n[7], base, 0.1),
            road_label: theme.text_dim,
            route: theme.accent_primary,
            route_casing: draw::mix(theme.accent_primary, Color::BLACK, 0.5),
            marker_ring: draw::mix(base, Color::WHITE, 0.85),
        }
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
///   2. paint a cached ancestor (lower-z) tile stretched under the hole so a
///      fresh zoom shows imagery *instantly* instead of waiting on I/O,
///   3. paint the exact tile on top if it is already decoded in memory,
///   4. otherwise *request* it from the background worker and move on.
///
/// Nothing here ever blocks on the network or on image decode — step 4 returns
/// immediately. Returns `true` if at least one exact tile was painted, so the
/// caller can fall back to the stylised vector basemap when no real imagery is
/// available yet (e.g. headless `--shots`, or the very first frame offline).
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

    let mut painted_exact = false;
    let mut pending: Vec<crate::tiles::TileId> = Vec::new();

    for (id, rect) in &visible {
        // (1) land underlay.
        tiles::paint_placeholder(scene, t, *rect, pal.land);

        // (2) ancestor placeholder (only needed when the exact tile is absent).
        let have_exact = tiles_layer.get(*id).is_some();
        if !have_exact {
            if let Some((anc_id, levels)) = tiles_layer.ancestor_placeholder(*id) {
                if let Some(anc_img) = tiles_layer.get(anc_id).cloned() {
                    tiles::paint_ancestor(scene, t, &anc_img, anc_id, *id, levels, *rect);
                }
            }
            pending.push(*id);
        }

        // (3) exact tile on top.
        if let Some(img) = tiles_layer.get(*id).cloned() {
            tiles::paint_tile(scene, t, &img, *rect);
            painted_exact = true;
        }
    }
    scene.pop_layer();

    // (4) request the missing tiles — non-blocking; results arrive via drain().
    for id in pending {
        tiles_layer.request(id);
    }

    painted_exact
}

/// Paint a reference-grid substrate aligned to the slippy-map tile pyramid.
///
/// Shown when there is neither real raster imagery nor a vector basemap yet —
/// the live app's first frames and any offline gap. Instead of a flat black
/// void, the viewport reads as a map substrate: land colour, a faint
/// tile-boundary grid, and a lighter quarter-tile sub-grid for texture. The
/// grid uses the exact same projection as [`draw_tiles`], so it lines up with
/// the imagery that fades in on top of it.
pub fn draw_grid_substrate(
    scene: &mut Scene,
    t: Affine,
    clip: Rect,
    model: &MapModel,
    view_center: Point,
    pal: &MapPalette,
) {
    draw::fill_rect(scene, t, clip, pal.land);
    let tiles = TileLayer::visible_tiles(model.center, model.zoom, view_center, clip);
    let major = draw::mix(pal.land, pal.road_secondary, 0.6);
    let minor = draw::mix(pal.land, pal.road_secondary, 0.28);
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
    // otherwise cover a bottom-left credit); OSM requires it stay visible.
    let attr = "© OpenStreetMap contributors";
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
    use crate::model::MapModel;
    use crate::tiles::TileLayer;

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
}
