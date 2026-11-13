//! The map layer: projects and paints the vector basemap, the route polyline and the place markers
//! into a Vello scene.

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{BlendMode, Color};

use ui::paint::PaintCtx;
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::tokens::colors::PresenceHue;

use crate::basemap::{Area, AreaKind, Basemap, RoadClass};
use crate::draw;
use crate::geo::Projection;
use crate::model::{Category, MapModel, Poi, TransitJourney, TransitMode};
use crate::tiles::{self, TileLayer};

/// Slippy-map zoom at and above which POI markers upgrade from small category dots to labelled icon
/// markers bearing a category pictogram. Kept here so the draw pass and the app's hit-test agree on
/// the marker size.
pub const POI_ICON_ZOOM: f64 = 16.0;

#[derive(Clone, Copy)]
pub struct MapPalette {
    pub marker_scale: f64,
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
    pub route: Color,
    pub route_casing: Color,
    pub marker_ring: Color,
    pub shadow: Color,
    pub label_plate: Color,
    pub label_ink: Color,
    /// The pre-tiles loading surface: a dark neutral from the theme, so the map never flashes
    /// bright before imagery arrives.
    pub substrate: Color,
    pub substrate_grid: Color,
    pub traffic_free: Color,
    pub traffic_moderate: Color,
    pub traffic_heavy: Color,
    pub traffic_severe: Color,
    pub traffic_casing: Color,
}

impl MapPalette {
    /// The cartographic palette from the theme, so the vector fallback, the tile land underlay and
    /// the loading substrate share one tone. The style is the one the raster layer decodes with
    /// ([`crate::tiles::TileStyle::from_theme`]), so drawn colours and decoded tones agree.
    pub fn from_theme(theme: &Theme) -> Self {
        use crate::tiles::TileStyle;
        if TileStyle::from_theme(theme) == TileStyle::Dark {
            Self::dark(theme)
        } else {
            Self::light(theme)
        }
    }

    fn light(theme: &Theme) -> Self {
        let c = &theme.tokens.colors;
        let sem = &c.semantic;
        // The media pair is the same near-white and near-black in every theme; the cartography
        // mixes toward them, never toward a widget ink.
        let w = c.media.ink;
        let k = c.media.letterbox;
        let land = draw::mix(c.surfaces.content, w, 0.94);
        Self {
            marker_scale: f64::from(theme.ui_scale),
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
            route: c.intent.you,
            route_casing: draw::mix(c.intent.you, w, 0.5),
            marker_ring: w,
            shadow: c.surfaces.scrim,
            label_plate: draw::alpha(land, 0.82),
            label_ink: draw::mix(land, k, 0.86),
            // The loading substrate shares the land tone, a faint step darker for the reference
            // grid, so a first-run view never seams.
            substrate: draw::mix(land, k, 0.05),
            substrate_grid: draw::mix(land, k, 0.14),
            traffic_free: sem.success,
            traffic_moderate: sem.warning,
            traffic_heavy: draw::mix(sem.warning, sem.danger, 0.55),
            traffic_severe: sem.danger,
            traffic_casing: draw::mix(land, k, 0.42),
        }
    }

    fn dark(theme: &Theme) -> Self {
        let c = &theme.tokens.colors;
        let sem = &c.semantic;
        let w = c.media.ink;
        let k = c.media.letterbox;
        let land = draw::mix(c.surfaces.content, k, 0.28);
        Self {
            marker_scale: f64::from(theme.ui_scale),
            land,
            building: draw::mix(land, w, 0.07),
            building_edge: draw::mix(land, w, 0.14),
            park: draw::mix(land, sem.success, 0.22),
            park_edge: draw::mix(land, sem.success, 0.36),
            water: draw::mix(land, sem.info, 0.30),
            road_res: draw::mix(land, w, 0.22),
            road_res_casing: draw::mix(land, k, 0.5),
            road_secondary: draw::mix(land, sem.warning, 0.34),
            road_primary: draw::mix(land, sem.warning, 0.52),
            route: c.intent.you,
            route_casing: draw::mix(c.intent.you, k, 0.45),
            marker_ring: draw::mix(land, w, 0.9),
            shadow: c.surfaces.scrim,
            label_plate: draw::alpha(land, 0.82),
            label_ink: draw::mix(land, w, 0.9),
            substrate: draw::mix(land, k, 0.18),
            substrate_grid: draw::mix(land, w, 0.10),
            traffic_free: sem.success,
            traffic_moderate: sem.warning,
            traffic_heavy: draw::mix(sem.warning, sem.danger, 0.55),
            traffic_severe: sem.danger,
            traffic_casing: draw::mix(land, w, 0.22),
        }
    }
}

pub fn traffic_color(pal: &MapPalette, level: crate::traffic::Congestion) -> Color {
    use crate::traffic::Congestion::{Free, Heavy, Moderate, Severe};
    match level {
        Free => pal.traffic_free,
        Moderate => pal.traffic_moderate,
        Heavy => pal.traffic_heavy,
        Severe => pal.traffic_severe,
    }
}

pub fn category_color(theme: &Theme, cat: Category) -> Color {
    let c = &theme.tokens.colors;
    let hue = match cat {
        Category::Landmark => PresenceHue::Red,
        Category::Hospital => PresenceHue::Pink,
        Category::Cafe => PresenceHue::Amber,
        Category::Shop => PresenceHue::Sky,
        Category::Restaurant => PresenceHue::Orange,
        Category::Bar => PresenceHue::Magenta,
        Category::Museum => PresenceHue::Violet,
        Category::Park => PresenceHue::Green,
        Category::Transit => PresenceHue::Blue,
        Category::Hotel => PresenceHue::Teal,
        Category::Other => return theme.text_dim,
    };
    c.presence.hue(hue)
}

/// Marker draw priority for decluttering: a more salient category keeps a contested screen cell, so
/// a dense area shows landmarks and transit stops, not scattered shops.
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

/// Choose which POIs to draw without piling markers on one another: a greedy pass in descending
/// priority keeps a POI only when no kept marker sits within `min_px`, and the selected POI is
/// always kept. Returns kept indices in input order; pure, so the draw pass and the click hit-test
/// agree.
pub fn declutter_indices(
    proj: &Projection,
    pois: &[Poi],
    min_px: f64,
    selected: Option<&str>,
) -> Vec<usize> {
    let pts: Vec<Point> = pois.iter().map(|p| proj.project(p.pos)).collect();
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

fn project_pts(proj: &Projection, pts: &[crate::model::LatLon]) -> Vec<Point> {
    pts.iter().map(|p| proj.project(*p)).collect()
}

fn road_width(class: RoadClass, zoom: f64) -> f64 {
    let k = ((zoom - 12.0).max(0.0)) * 0.35 + 1.0;
    match class {
        RoadClass::Primary => 4.2 * k,
        RoadClass::Secondary => 3.0 * k,
        RoadClass::Residential => 1.6 * k,
    }
}

/// Paint the tiles covering the viewport: land colour, a cached ancestor scaled under the hole,
/// cached descendants into it, the exact tile when decoded, else a request that returns at once.
/// Never blocks, so zoom keeps showing imagery. Returns whether any imagery painted; the caller
/// falls back to the vector basemap only when the viewport is genuinely empty.
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
        tiles::paint_placeholder(scene, t, *rect, pal.land);

        let have_exact = tiles_layer.get(*id).is_some();
        if !have_exact {
            if let Some((anc_id, levels)) = tiles_layer.ancestor_placeholder(*id)
                && let Some(anc_img) = tiles_layer.get(anc_id).cloned()
            {
                tiles::paint_ancestor(scene, t, &anc_img, anc_id, *id, levels, *rect);
                painted_any = true;
            }
            if tiles::paint_descendants(scene, t, tiles_layer, *id, *rect, 2) {
                painted_any = true;
            }
            pending.push(*id);
        }

        if let Some(img) = tiles_layer.get(*id).cloned() {
            tiles::paint_tile(scene, t, &img, *rect);
            painted_any = true;
        }
    }
    scene.pop_layer();

    for id in pending {
        tiles_layer.request(id);
    }

    painted_any
}

/// Paint a reference-grid substrate aligned to the tile pyramid, for the frames before imagery or a
/// vector basemap: a calm dark surface with a faint tile grid, so a first-run map reads as a map,
/// not a void.
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

pub fn draw_basemap(
    scene: &mut Scene,
    t: Affine,
    clip: Rect,
    proj: &Projection,
    base: &Basemap,
    pal: &MapPalette,
) {
    draw::fill_rect(scene, t, clip, pal.land);

    for a in base.areas.iter().filter(|a| a.kind == AreaKind::Water) {
        draw_area(scene, t, proj, a, pal.water, None);
    }
    for a in base.areas.iter().filter(|a| a.kind == AreaKind::Park) {
        draw_area(scene, t, proj, a, pal.park, Some(pal.park_edge));
    }

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
            RoadClass::Residential => pal.road_res,
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
        let mut ring = pts.clone();
        if let Some(first) = pts.first() {
            ring.push(*first);
        }
        draw::polyline(scene, t, &ring, 1.0, e);
    }
}

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
    let scale = pal.marker_scale;
    draw::polyline(scene, t, &pts, 9.0 * scale, pal.route_casing);
    draw::polyline(scene, t, &pts, 5.5 * scale, pal.route);

    let (Some(&start), Some(&end)) = (pts.first(), pts.last()) else {
        return;
    };
    draw::fill_circle(scene, t, start, 7.0 * scale, pal.marker_ring);
    draw::stroke_circle(scene, t, start, 7.0 * scale, 3.0 * scale, pal.route);
    draw::fill_circle(scene, t, end, 7.0 * scale, pal.route);
    draw::stroke_circle(scene, t, end, 7.0 * scale, 2.5 * scale, pal.marker_ring);
}

pub fn draw_endpoint_marker(
    scene: &mut Scene,
    t: Affine,
    center: Point,
    pal: &MapPalette,
    destination: bool,
) {
    let t = scaled_at(t, center, pal.marker_scale);
    if destination {
        draw::fill_circle(scene, t, center, 6.0, pal.route);
        draw::stroke_circle(scene, t, center, 6.0, 2.0, pal.marker_ring);
    } else {
        draw::fill_circle(scene, t, center, 6.0, pal.marker_ring);
        draw::stroke_circle(scene, t, center, 6.0, 2.5, pal.route);
    }
}

/// Paint the traffic overlay under the route and markers; empty data draws nothing.
pub fn draw_traffic(
    scene: &mut Scene,
    t: Affine,
    proj: &Projection,
    traffic: &crate::traffic::TrafficData,
    pal: &MapPalette,
) {
    use crate::traffic::Congestion;
    let k = ((proj.zoom() - 12.0).max(0.0)) * 0.35 + 1.0;
    let width = |level: Congestion| {
        k * match level {
            Congestion::Free => 2.2,
            Congestion::Moderate => 2.7,
            Congestion::Heavy => 3.1,
            Congestion::Severe => 3.4,
        }
    };
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

pub fn transit_leg_color(theme: &Theme, mode: TransitMode) -> Color {
    let c = &theme.tokens.colors;
    let hue = match mode {
        TransitMode::Walk => return c.text_dim,
        TransitMode::Bus => PresenceHue::Teal,
        TransitMode::Tram => PresenceHue::Red,
        TransitMode::Subway => PresenceHue::Blue,
        TransitMode::Rail => PresenceHue::Green,
        TransitMode::Ferry => PresenceHue::Cyan,
    };
    c.presence.hue(hue)
}

/// The GTFS line colour when the feed has one, else the theme's per-mode colour, so on-map legs
/// match the panel's pills.
pub fn leg_color(theme: &Theme, leg: &crate::model::TransitLeg) -> Color {
    leg.color
        .as_deref()
        .and_then(draw::hex_color)
        .unwrap_or_else(|| transit_leg_color(theme, leg.mode))
}

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
    let scale = f64::from(theme.ui_scale);
    for leg in &journey.legs {
        if leg.geometry.len() < 2 {
            continue;
        }
        let pts = project_pts(proj, &leg.geometry);
        let col = leg_color(theme, leg);
        if leg.mode == TransitMode::Walk {
            draw_dashed(
                scene,
                t,
                &pts,
                3.0 * scale,
                draw::alpha(col, 0.9),
                6.0 * scale,
                5.0 * scale,
            );
        } else {
            draw::polyline(
                scene,
                t,
                &pts,
                8.0 * scale,
                draw::mix(col, theme.tokens.colors.media.letterbox, 0.45),
            );
            draw::polyline(scene, t, &pts, 5.0 * scale, col);
        }
    }
    for leg in &journey.legs {
        if leg.mode == TransitMode::Walk {
            continue;
        }
        let pts = project_pts(proj, &leg.geometry);
        let col = leg_color(theme, leg);
        for anchor in [pts.first(), pts.last()].into_iter().flatten() {
            if clip.contains(*anchor) {
                draw::fill_circle(scene, t, *anchor, 5.0 * scale, pal.marker_ring);
                draw::stroke_circle(scene, t, *anchor, 5.0 * scale, 2.5 * scale, col);
            }
        }
    }
    scene.pop_layer();
}

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

#[derive(Clone, Copy)]
pub struct MapView<'a> {
    pub proj: &'a Projection,
    pub pal: &'a MapPalette,
    pub clip: Rect,
    /// The floating surfaces over the map, each grown by the gap a marker keeps from it; a marker
    /// or name that would reach under one is left out.
    pub chrome: &'a [Rect],
}

impl MapView<'_> {
    pub fn clear_of_chrome(&self, bounds: Rect) -> bool {
        self.chrome
            .iter()
            .all(|surface| surface.intersect(bounds).area() <= 0.0)
    }

    pub fn shows_whole(&self, plate: Rect) -> bool {
        plate.x0 >= self.clip.x0
            && plate.x1 <= self.clip.x1
            && plate.y0 >= self.clip.y0
            && plate.y1 <= self.clip.y1
            && self.clear_of_chrome(plate)
    }
}

pub fn draw_markers(ctx: &mut PaintCtx<'_>, view: MapView<'_>, model: &MapModel) {
    let MapView {
        proj, pal, clip, ..
    } = view;
    let (t, theme) = (ctx.transform, ctx.theme);
    let selected = model.selected.clone();
    ctx.scene.push_layer(
        vello::peniko::Fill::NonZero,
        BlendMode::default(),
        1.0,
        t,
        &clip,
    );
    if let Some(o) = model.origin {
        let p = proj.project(o);
        let scale = pal.marker_scale;
        let bounds = Rect::from_center_size(p, (16.0 * scale, 16.0 * scale));
        if view.shows_whole(bounds) {
            let scene = &mut *ctx.scene;
            let t = scaled_at(t, p, scale);
            draw::fill_circle(scene, t, Point::new(p.x, p.y + 1.0), 6.5, pal.shadow);
            draw::fill_circle(scene, t, p, 6.0, pal.marker_ring);
            draw::stroke_circle(scene, t, p, 6.0, 3.0, pal.route);
            draw::fill_circle(scene, t, p, 2.2, pal.route);
        }
    }

    for place in &model.places {
        if Some(&place.id) == selected.as_ref() {
            continue;
        }
        let p = proj.project(place.pos);
        if !view.shows_whole(pin_bounds(p, false, pal.marker_scale)) {
            continue;
        }
        draw_pin(
            ctx.scene,
            t,
            p,
            category_color(theme, place.category),
            pal,
            false,
        );
    }

    if let Some(sel) = model.selected_place() {
        let p = proj.project(sel.pos);
        let pin = pin_bounds(p, true, pal.marker_scale);
        if view.shows_whole(pin) {
            draw_pin(
                ctx.scene,
                t,
                p,
                category_color(theme, sel.category),
                pal,
                true,
            );
            let style = theme.text(TextRole::Caption).strong();
            let label = &sel.name;
            let sp = &theme.tokens.spacing;
            let pad = f64::from(sp.s2);
            let rect = place_label_rect(ctx, p, label);
            if view.shows_whole(rect) {
                draw::fill_round(
                    ctx.scene,
                    t,
                    rect,
                    f64::from(theme.tokens.radii.detail),
                    pal.label_plate,
                );
                let top = draw::vmid(ctx.text, rect.y0, rect.height(), &style);
                draw::text(
                    ctx,
                    label,
                    Point::new(rect.x0 + pad, top),
                    &style,
                    pal.label_ink,
                );
            }
        }
    }
    ctx.scene.pop_layer();
}

/// Draw the POI layer over the basemap: category dots far out, icon discs and a name plate closer
/// in, the selected POI enlarged with a halo. `visible` is the decluttered set
/// ([`declutter_indices`]).
pub fn draw_pois(
    ctx: &mut PaintCtx<'_>,
    view: MapView<'_>,
    pois: &[Poi],
    visible: &[usize],
    selected: Option<&str>,
    places: &MapModel,
) {
    let MapView {
        proj, clip, pal, ..
    } = view;
    let mut labels = LabelPlacer {
        occupied: places
            .places
            .iter()
            .map(|place| {
                pin_bounds(
                    proj.project(place.pos),
                    places.selected.as_deref() == Some(&place.id),
                    pal.marker_scale,
                )
            })
            .collect(),
        gap: f64::from(ctx.theme.tokens.spacing.s1),
    };
    if let Some(place) = places.selected_place() {
        labels
            .occupied
            .push(place_label_rect(ctx, proj.project(place.pos), &place.name));
    }
    for &i in visible {
        if let Some(poi) = pois.get(i) {
            let radius = poi_radius(proj.zoom() >= POI_ICON_ZOOM, selected == Some(&poi.id))
                * pal.marker_scale;
            labels.occupied.push(Rect::from_center_size(
                proj.project(poi.pos),
                (radius * 2.0, radius * 2.0),
            ));
        }
    }
    ctx.scene.push_layer(
        vello::peniko::Fill::NonZero,
        BlendMode::default(),
        1.0,
        ctx.transform,
        &clip,
    );
    if let Some(sel) = selected
        && let Some(poi) = pois.iter().find(|p| p.id == sel)
        && clip.contains(proj.project(poi.pos))
    {
        draw_poi_dot(ctx, view, poi, true, &mut labels);
    }
    for &i in visible {
        let Some(poi) = pois.get(i) else {
            continue;
        };
        if selected == Some(poi.id.as_str()) {
            continue;
        }
        if !clip.contains(proj.project(poi.pos)) {
            continue;
        }
        draw_poi_dot(ctx, view, poi, false, &mut labels);
    }
    ctx.scene.pop_layer();
}

fn place_label_rect(ctx: &mut PaintCtx<'_>, anchor: Point, name: &str) -> Rect {
    let theme = ctx.theme;
    let style = theme.text(TextRole::Caption).strong();
    let pad = f64::from(theme.tokens.spacing.s2);
    let height = draw::line_h(ctx.text, &style) + f64::from(theme.tokens.spacing.s1);
    let width = draw::measure_w(ctx.text, name, &style) + 2.0 * pad;
    let (radius, head) = pin_size(true, f64::from(theme.ui_scale));
    let top = anchor.y - head - radius - f64::from(theme.tokens.spacing.s1) - height;
    Rect::new(
        anchor.x - width / 2.0,
        top,
        anchor.x + width / 2.0,
        top + height,
    )
}

/// Name placement: either side, then below; only the name is dropped when none fits.
struct LabelPlacer {
    occupied: Vec<Rect>,
    gap: f64,
}

impl LabelPlacer {
    fn place(
        &mut self,
        view: MapView<'_>,
        right: Rect,
        anchor: Point,
        radius: f64,
    ) -> Option<Rect> {
        let left = Rect::new(
            2.0 * anchor.x - right.x1,
            right.y0,
            2.0 * anchor.x - right.x0,
            right.y1,
        );
        let below = Rect::from_origin_size(
            (
                anchor.x - right.width() / 2.0,
                (anchor.y + radius + self.gap).max(right.y1 + self.gap),
            ),
            right.size(),
        );
        let plate = [right, left, below].into_iter().find(|plate| {
            view.shows_whole(*plate)
                && self
                    .occupied
                    .iter()
                    .all(|other| other.inflate(self.gap, self.gap).intersect(*plate).area() <= 0.0)
        })?;
        self.occupied.push(plate);
        Some(plate)
    }
}

pub(crate) fn poi_radius(icons: bool, selected: bool) -> f64 {
    match (icons, selected) {
        (true, true) => 13.0,
        (true, false) => 9.0,
        (false, true) => 8.5,
        (false, false) => 4.5,
    }
}

fn draw_poi_dot(
    ctx: &mut PaintCtx<'_>,
    view: MapView<'_>,
    poi: &Poi,
    selected: bool,
    labels: &mut LabelPlacer,
) {
    let MapView { proj, pal, .. } = view;
    let (scene, t, theme) = (&mut *ctx.scene, ctx.transform, ctx.theme);
    let p = proj.project(poi.pos);
    let icons = proj.zoom() >= POI_ICON_ZOOM;
    let label = selected || proj.zoom() >= 16.5;
    let col = category_color(theme, poi.category);
    let scale = f64::from(theme.ui_scale);
    let r = poi_radius(icons, selected) * scale;
    let reach = r + (if selected { 5.0 } else { 2.0 }) * scale;
    if !view.shows_whole(Rect::from_center_size(p, (2.0 * reach, 2.0 * reach))) {
        return;
    }
    draw::fill_circle(
        scene,
        t,
        Point::new(p.x, p.y + 0.9 * scale),
        r + 1.3 * scale,
        pal.shadow,
    );
    draw::fill_circle(scene, t, p, r, col);
    draw::stroke_circle(
        scene,
        t,
        p,
        r,
        (if selected { 2.5 } else { 1.6 }) * scale,
        pal.marker_ring,
    );
    if icons {
        draw_category_icon(scene, t, p, r, poi.category, pal.marker_ring);
    }
    if selected {
        draw::stroke_circle(
            scene,
            t,
            p,
            r + 4.0 * scale,
            2.0 * scale,
            draw::alpha(col, 0.5),
        );
    }
    if label {
        let style = if selected {
            theme.text(TextRole::Caption).strong()
        } else {
            theme.text(TextRole::Caption)
        };
        let tw = draw::measure_w(ctx.text, &poi.name, &style);
        let lh = draw::line_h(ctx.text, &style);
        let s1 = f64::from(theme.tokens.spacing.s1);
        let lx = p.x + r + f64::from(theme.tokens.spacing.s2);
        let ly = p.y - lh / 2.0;
        let plate = Rect::new(lx - s1, ly, lx + tw + s1, ly + lh);
        // A name plate shows only whole, never sliced at the window edge or under a floating
        // surface.
        if let Some(plate) = labels.place(view, plate, p, r) {
            draw::fill_round(
                ctx.scene,
                t,
                plate,
                f64::from(theme.tokens.radii.detail),
                pal.label_plate,
            );
            draw::text(
                ctx,
                &poi.name,
                Point::new(plate.x0 + s1, plate.y0),
                &style,
                pal.label_ink,
            );
        }
    }
}

pub fn draw_category_icon(
    scene: &mut Scene,
    t: Affine,
    c: Point,
    r: f64,
    cat: Category,
    fg: Color,
) {
    let side = r * 1.25;
    let stroke = (r * 0.16).max(1.4);
    cat.icon().draw_with_stroke(
        scene,
        t,
        Rect::from_center_size(c, (side, side)),
        fg,
        stroke,
    );
}

/// A teardrop pin: a head of radius `head_r` a `height` above `anchor`, its sides tangent down to
/// the anchor. One closed path, so fill and outline have no seam.
fn pin_path(anchor: Point, head_r: f64, height: f64) -> vello::kurbo::BezPath {
    use std::f64::consts::{FRAC_PI_2, PI};
    let cx = anchor.x;
    let cy = anchor.y - height;
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

pub(crate) fn pin_size(selected: bool, scale: f64) -> (f64, f64) {
    let (radius, height) = if selected { (11.0, 30.0) } else { (8.0, 22.0) };
    (radius * scale, height * scale)
}

fn scaled_at(t: Affine, anchor: Point, scale: f64) -> Affine {
    t * Affine::translate(anchor.to_vec2())
        * Affine::scale(scale)
        * Affine::translate(-anchor.to_vec2())
}

fn pin_bounds(anchor: Point, selected: bool, scale: f64) -> Rect {
    let (r, h) = pin_size(selected, scale);
    let reach = r + (if selected { 6.0 } else { 2.0 }) * scale;
    Rect::new(
        anchor.x - reach,
        anchor.y - h - reach,
        anchor.x + reach,
        anchor.y + r * 0.24,
    )
}

fn draw_pin(
    scene: &mut Scene,
    t: Affine,
    anchor: Point,
    color: Color,
    pal: &MapPalette,
    selected: bool,
) {
    let (r, h) = pin_size(selected, 1.0);
    let t = scaled_at(t, anchor, pal.marker_scale);
    let center = Point::new(anchor.x, anchor.y - h);

    let shadow = vello::kurbo::Ellipse::new(anchor, (r * 0.62, r * 0.24), 0.0);
    scene.fill(vello::peniko::Fill::NonZero, t, pal.shadow, None, &shadow);

    if selected {
        draw::fill_circle(scene, t, center, r + 6.0, draw::alpha(color, 0.20));
    }

    let body = pin_path(anchor, r, h);
    draw::stroke_path(
        scene,
        t,
        &body,
        if selected { 3.0 } else { 2.4 },
        pal.marker_ring,
    );
    draw::fill_path(scene, t, &body, color);

    draw::fill_circle(scene, t, center, r * 0.40, pal.marker_ring);
    draw::fill_circle(scene, t, center, r * 0.20, draw::alpha(color, 0.9));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{LatLon, MapModel, Poi};
    use crate::tiles::TileLayer;
    use ui::text_render::TextRenderer;

    #[test]
    fn labels_try_clear_alternatives_and_never_cover_markers_or_names() {
        let theme = Theme::dark().with_ui_scale(1.5);
        let pal = MapPalette::from_theme(&theme);
        let proj = Projection::new(LatLon::new(48.137, 11.575), 17.0, Point::new(500.0, 400.0));
        let chrome = [Rect::new(600.0, 0.0, 800.0, 810.0)];
        let view = MapView {
            proj: &proj,
            pal: &pal,
            clip: Rect::new(0.0, 0.0, 1440.0, 810.0),
            chrome: &chrome,
        };
        let marker = Rect::from_center_size((500.0, 400.0), (27.0, 27.0));
        let mut labels = LabelPlacer {
            occupied: vec![marker],
            gap: 6.0,
        };
        let right = Rect::new(526.0, 385.0, 686.0, 415.0);
        let left = labels
            .place(view, right, Point::new(500.0, 400.0), 13.5)
            .expect("left side fits");
        assert!(left.x1 < marker.x0);
        let next = labels
            .place(view, right, Point::new(500.0, 400.0), 13.5)
            .expect("below fits");
        assert!(next.y0 > marker.y1);
        assert_eq!(left.intersect(next).area(), 0.0);
        assert!(
            labels
                .place(view, right, Point::new(500.0, 400.0), 13.5)
                .is_none()
        );
        for plate in [left, next] {
            assert!(view.shows_whole(plate));
            assert_eq!(marker.intersect(plate).area(), 0.0);
        }
    }

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
        {
            let center = LatLon::new(48.1371, 11.5754);
            let proj = Projection::new(center, 17.0, Point::new(640.0, 400.0));

            let far = LatLon::new(center.lat, center.lon + 0.01);
            let pois = vec![
                poi_at("shop", Category::Shop, center.lat, center.lon),
                poi_at("transit", Category::Transit, center.lat, center.lon + 1e-6),
                poi_at("cafe-far", Category::Cafe, far.lat, far.lon),
            ];

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

            let mut sorted = kept.clone();
            sorted.sort_unstable();
            assert_eq!(kept, sorted, "kept indices preserve input order");

            let kept_sel = declutter_indices(&proj, &pois, 24.0, Some("shop"));
            let ids_sel: Vec<&str> = kept_sel.iter().map(|&i| pois[i].id.as_str()).collect();
            assert!(
                ids_sel.contains(&"shop"),
                "the selected POI is always kept: {ids_sel:?}"
            );
        }

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
            assert!(
                (lum(dark.road_res) - lum(dark.land)).abs() > 30.0,
                "dark residential roads must read against the dark land"
            );
            assert!(
                (lum(light.road_primary) - lum(light.land)).abs() > 15.0,
                "light arterials must read against the paper land"
            );
        }

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

        {
            use vello::kurbo::Shape as _;
            let anchor = Point::new(100.0, 200.0);
            let (r, h) = (8.0, 22.0);
            let bb = pin_path(anchor, r, h).bounding_box();
            assert!(
                (bb.y1 - anchor.y).abs() < 0.5,
                "pin tip on the anchor, got y1={}",
                bb.y1
            );
            assert!(
                (bb.y0 - (anchor.y - h - r)).abs() < 1.5,
                "head top near anchor.y-h-r, got y0={}",
                bb.y0
            );
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
            assert_eq!(
                leg_color(&theme, &base),
                Color::from_rgba8(0xeb, 0x69, 0x0b, 255)
            );
            let no_color = TransitLeg {
                color: None,
                ..base.clone()
            };
            assert_eq!(
                leg_color(&theme, &no_color),
                transit_leg_color(&theme, TransitMode::Subway)
            );
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
            let paths = |chrome: &[Rect], tr: &mut TextRenderer| {
                let mut scene = Scene::new();
                let view = MapView {
                    proj: &proj,
                    pal: &pal,
                    clip,
                    chrome,
                };
                draw_markers(
                    &mut PaintCtx::new(&mut scene, Affine::IDENTITY, tr, &theme),
                    view,
                    &model,
                );
                scene.encoding().n_paths
            };
            assert!(
                clip.contains(proj.project(model.center)),
                "the marker is on screen"
            );
            let open = paths(&[], &mut tr);
            let none = paths(&[clip], &mut tr);
            assert!(open > none, "the pin and its name plate paint");
            let at = proj.project(model.center);
            let over = Rect::new(at.x - 4.0, at.y - 4.0, at.x + 200.0, at.y + 4.0);
            assert_eq!(paths(&[over], &mut tr), none, "a covered pin is left out");
            let plate = Rect::new(at.x - 2.0, at.y - 80.0, at.x + 2.0, at.y - 52.0);
            let covered = paths(&[plate], &mut tr);
            assert!(
                covered > none && covered < open,
                "the pin shows without its name"
            );
        }

        {
            let theme = Theme::dark();
            let pal = MapPalette::from_theme(&theme);
            let mut layer = TileLayer::offline();
            let model = MapModel::default();
            let clip = Rect::new(0.0, 0.0, 1280.0, 800.0);
            let view_center = clip.center();

            let mut scene = Scene::new();
            let painted_exact = draw_tiles(
                &mut scene,
                Affine::IDENTITY,
                clip,
                &mut layer,
                &model,
                view_center,
                &pal,
            );

            assert!(!painted_exact);
            assert_eq!(layer.mem_len(), 0, "draw must not decode tiles inline");
            assert!(!layer.has_inflight());
        }

        {
            let theme = Theme::dark();
            let pal = MapPalette::from_theme(&theme);
            let clip = Rect::new(0.0, 0.0, 1280.0, 800.0);
            let vc = clip.center();
            let center = crate::model::LatLon::new(48.1371, 11.5754);

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

            let mut empty = TileLayer::offline();
            assert!(!paints(&mut empty, 14.0), "empty cache paints no imagery");
        }
    }
}
