//! The map layer: projects and paints the stylised vector basemap, the
//! active route polyline, and the place markers into a Vello scene.
//!
//! Colours derive from the `Theme` via [`MapPalette`] so the cartography
//! tracks the OS theme (dark map under a dark theme, etc.) instead of
//! hard-coding a palette.

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Color;

use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;

use crate::draw;
use crate::geo::{self, Projection};
use crate::model::{Category, MapModel};
use crate::sample::{Area, AreaKind, Basemap, RoadClass};

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

    // Attribution, bottom-left.
    let attr = "© OpenStreetMap contributors";
    let aw = draw::measure_w(tr, attr, 10.0) + 14.0;
    let ax = clip.x0 + 12.0;
    let ay = clip.y1 - 22.0;
    draw::fill_rrect(
        scene,
        t,
        Rect::new(ax, ay, ax + aw, ay + 17.0),
        4.0,
        bar_bg,
    );
    draw::text(scene, tr, t, attr, ax + 7.0, ay + 3.0, 10.0, theme.text_dim);
}
