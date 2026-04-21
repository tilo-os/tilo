//! Lucide-icon glyph module (gap #5).
//!
//! The shell chrome used to paint its icons as ad-hoc kurbo primitives —
//! four filled squares for the overview glyph, a stroked ring + dot for
//! the "gear", three filled dots for the tray. That reads as a geometric
//! toy next to the concept's lucide-react glyph set. This module gives
//! widgets a single stroked-glyph helper so the shell matches the
//! concept's 1.5 px lucide aesthetic without every call site rolling
//! its own BezPath.
//!
//! # Design
//!
//! * Each [`Icon`] variant resolves to one or more [`BezPath`] subpaths
//!   traced from the upstream lucide-react source (v0.546.0, ISC
//!   licensed). Paths are stored in the lucide 24×24 viewBox; we scale
//!   and translate at draw time so the same [`Icon`] can render at any
//!   pixel size.
//! * Lucide icons are *stroked*, not filled — default attrs set
//!   `stroke-width: 2`, `stroke-linecap: round`, `stroke-linejoin:
//!   round`, `fill: none`. [`draw_icon`] mirrors that exactly.
//! * Some lucide icons are composed of multiple SVG primitives
//!   (`<rect>`, `<line>`, `<circle>`, `<path>`). We flatten those into
//!   a `Vec<BezPath>` up-front so the draw helper only has to walk
//!   strokable subpaths.
//! * This module intentionally stays small. No trait hierarchy, no
//!   icon-theme system, no per-icon caching — just a data table and
//!   one pure draw function. When a new icon is needed, add a variant
//!   + a row to [`icon_subpaths`].
//!
//! # Adding a new icon
//!
//! 1. Pull the `__iconNode` entries from
//!    `nirios-concept/node_modules/lucide-react/dist/esm/icons/<kebab>.js`
//!    (or directly from the lucide-icons repo).
//! 2. Add a variant to [`Icon`].
//! 3. Map it in [`icon_subpaths`] to a `Vec<BezPath>`; prefer
//!    [`path_from_d`] for raw `d=""` strings, and the `rect_path` /
//!    `circle_path` / `line_path` helpers for the non-path primitives.

use vello::Scene;
use vello::kurbo::{Affine, BezPath, Cap, Circle, Join, Point, Rect, RoundedRect, Shape, Stroke};
use vello::peniko::Color;

/// Stable identifier for a lucide glyph. New entries map 1:1 to lucide
/// icon kebab names (e.g. `Icon::LayoutGrid` → `layout-grid`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Icon {
    LayoutGrid,
    Settings2,
    Wifi,
    Monitor,
    Battery,
    Shield,
    Maximize2,
    Minimize2,
    X,
    ChevronUp,
    ChevronDown,
    Plus,
    Search,
}

/// Lucide's fixed viewBox. Every stored path assumes this coordinate
/// system; [`draw_icon`] scales to the requested rect.
pub const LUCIDE_VIEWBOX: f64 = 24.0;

/// Lucide's default stroke width (in viewBox units) — 2.0 maps to
/// 1.5 px after the concept's Tailwind tweaks, so we scale by 0.75
/// when translating viewBox stroke width to output pixels. Call sites
/// can still override via the `stroke_px` argument of [`draw_icon`].
pub const LUCIDE_DEFAULT_STROKE_PX: f64 = 1.5;

/// Parse a lucide `d="…"` string into a [`BezPath`].
///
/// Panics if the supplied string is malformed. All paths shipped in
/// this module are unit-tested to parse into non-empty bez paths, so a
/// panic here flags a bad hand-edit rather than a runtime bug.
fn path_from_d(d: &str) -> BezPath {
    BezPath::from_svg(d).expect("lucide path literal must parse")
}

/// Build the stroked outline of a lucide `<rect>` primitive with an
/// optional corner radius (`rx`). Matches the SVG spec's "the edge is
/// stroked, not filled".
fn rect_path(x: f64, y: f64, w: f64, h: f64, rx: f64) -> BezPath {
    let rect = Rect::new(x, y, x + w, y + h);
    if rx > 0.0 {
        // `RoundedRect::into_path` yields the closed outline we want.
        RoundedRect::from_rect(rect, rx).path_elements(0.1).collect()
    } else {
        let mut p = BezPath::new();
        p.move_to(Point::new(rect.x0, rect.y0));
        p.line_to(Point::new(rect.x1, rect.y0));
        p.line_to(Point::new(rect.x1, rect.y1));
        p.line_to(Point::new(rect.x0, rect.y1));
        p.close_path();
        p
    }
}

/// Build the stroked outline of a lucide `<line>` primitive.
fn line_path(x1: f64, y1: f64, x2: f64, y2: f64) -> BezPath {
    let mut p = BezPath::new();
    p.move_to(Point::new(x1, y1));
    p.line_to(Point::new(x2, y2));
    p
}

/// Build the stroked outline of a lucide `<circle>` primitive.
fn circle_path(cx: f64, cy: f64, r: f64) -> BezPath {
    Circle::new(Point::new(cx, cy), r).path_elements(0.1).collect()
}

/// Return the set of strokable subpaths for `icon` in the lucide 24×24
/// viewBox. Widgets normally call [`draw_icon`] rather than inspecting
/// this directly; it's `pub(crate)` so unit tests can poke at the raw
/// shapes without duplicating the data table.
pub(crate) fn icon_subpaths(icon: Icon) -> Vec<BezPath> {
    match icon {
        // `__iconNode` is four 7×7 rects with rx=1 at the corners of a
        // 3/14 grid. We emit the rounded-rect outlines directly.
        Icon::LayoutGrid => vec![
            rect_path(3.0, 3.0, 7.0, 7.0, 1.0),
            rect_path(14.0, 3.0, 7.0, 7.0, 1.0),
            rect_path(14.0, 14.0, 7.0, 7.0, 1.0),
            rect_path(3.0, 14.0, 7.0, 7.0, 1.0),
        ],
        // Two horizontal rails + two ringed dots on opposite corners.
        Icon::Settings2 => vec![
            path_from_d("M14 17H5"),
            path_from_d("M19 7h-9"),
            circle_path(17.0, 17.0, 3.0),
            circle_path(7.0, 7.0, 3.0),
        ],
        // Three arcs + a dot at the base.
        Icon::Wifi => vec![
            path_from_d("M12 20h.01"),
            path_from_d("M2 8.82a15 15 0 0 1 20 0"),
            path_from_d("M5 12.859a10 10 0 0 1 14 0"),
            path_from_d("M8.5 16.429a5 5 0 0 1 7 0"),
        ],
        // Screen rect + stand.
        Icon::Monitor => vec![
            rect_path(2.0, 3.0, 20.0, 14.0, 2.0),
            line_path(8.0, 21.0, 16.0, 21.0),
            line_path(12.0, 17.0, 12.0, 21.0),
        ],
        // Body rect + terminal nub.
        Icon::Battery => vec![
            path_from_d("M 22 14 L 22 10"),
            rect_path(2.0, 6.0, 16.0, 12.0, 2.0),
        ],
        // Classic heraldic shield outline (single path).
        Icon::Shield => vec![path_from_d(
            "M20 13c0 5-3.5 7.5-7.66 8.95a1 1 0 0 1-.67-.01C7.5 20.5 4 18 4 13V6a1 1 0 0 1 1-1c2 0 4.5-1.2 6.24-2.72a1.17 1.17 0 0 1 1.52 0C14.51 3.81 17 5 19 5a1 1 0 0 1 1 1z",
        )],
        // Two opposing L-brackets + a diagonal in each.
        Icon::Maximize2 => vec![
            path_from_d("M15 3h6v6"),
            path_from_d("m21 3-7 7"),
            path_from_d("m3 21 7-7"),
            path_from_d("M9 21H3v-6"),
        ],
        Icon::Minimize2 => vec![
            path_from_d("m14 10 7-7"),
            path_from_d("M20 10h-6V4"),
            path_from_d("m3 21 7-7"),
            path_from_d("M4 14h6v6"),
        ],
        Icon::X => vec![
            path_from_d("M18 6 6 18"),
            path_from_d("m6 6 12 12"),
        ],
        Icon::ChevronUp => vec![path_from_d("m18 15-6-6-6 6")],
        Icon::ChevronDown => vec![path_from_d("m6 9 6 6 6-6")],
        Icon::Plus => vec![
            path_from_d("M5 12h14"),
            path_from_d("M12 5v14"),
        ],
        // Magnifier: circle r=8 at (11, 11) + diagonal handle line
        // (21, 21) → (16.65, 16.65). Transcribed verbatim from
        // lucide's `search.js` (v0.546.0).
        Icon::Search => vec![
            circle_path(11.0, 11.0, 8.0),
            path_from_d("m21 21-4.3-4.3"),
        ],
    }
}

/// Paint `icon` stroked in `color` into `scene`, centred within `rect`.
///
/// * `scale_px` is derived from `rect.size().min() / LUCIDE_VIEWBOX` so
///   the icon always fits the shorter dimension; rectangular cells
///   pad the longer axis.
/// * `stroke_px` is the **output** stroke width in pixels; lucide
///   renders at 2 px on a 24 unit viewBox, the concept trims that to
///   1.5 px — pass [`LUCIDE_DEFAULT_STROKE_PX`] for parity or override
///   per call site. The stroke is divided by `scale_px` before being
///   handed to vello so the stroked path remains consistent across
///   icon sizes.
/// * Caps and joins are rounded, matching lucide defaults.
///
/// This function is intentionally `&Scene`-mutating only; it takes no
/// `&mut self` and touches no state outside `scene`.
pub fn draw_icon(
    scene: &mut Scene,
    transform: Affine,
    rect: Rect,
    icon: Icon,
    color: Color,
    stroke_px: f64,
) {
    let size = rect.size();
    let min = size.width.min(size.height);
    if min <= 0.0 {
        return;
    }
    let scale = min / LUCIDE_VIEWBOX;
    // Center the 24×24 glyph inside `rect` when the target is not
    // square. This is what lets a 14×14 icon slot next to 16×22 chrome
    // without a custom transform at each call site.
    let glyph_w = LUCIDE_VIEWBOX * scale;
    let glyph_h = LUCIDE_VIEWBOX * scale;
    let tx = rect.x0 + (size.width - glyph_w) / 2.0;
    let ty = rect.y0 + (size.height - glyph_h) / 2.0;
    let local = transform
        * Affine::translate((tx, ty))
        * Affine::scale(scale);

    let stroke = Stroke::new(stroke_px / scale)
        .with_caps(Cap::Round)
        .with_join(Join::Round);

    for sub in icon_subpaths(icon) {
        scene.stroke(&stroke, local, color, None, &sub);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Walk a path's elements; returns the count so tests can assert
    /// "non-empty" without matching against internal enum shapes.
    fn element_count(p: &BezPath) -> usize {
        p.elements().iter().count()
    }

    #[test]
    fn icon_layout_grid_parses_to_non_empty_bezpath() {
        let sub = icon_subpaths(Icon::LayoutGrid);
        assert_eq!(sub.len(), 4, "layout-grid has 4 rect subpaths");
        for p in &sub {
            assert!(element_count(p) > 0, "subpath must emit path elements");
        }
    }

    #[test]
    fn icon_settings2_parses_to_non_empty_bezpath() {
        let sub = icon_subpaths(Icon::Settings2);
        assert_eq!(sub.len(), 4, "settings-2 has 2 paths + 2 circles");
        for p in &sub {
            assert!(element_count(p) > 0);
        }
    }

    #[test]
    fn icon_shield_parses_to_non_empty_bezpath() {
        let sub = icon_subpaths(Icon::Shield);
        assert_eq!(sub.len(), 1);
        // The shield path has multiple curve_to + line_to instructions.
        assert!(element_count(&sub[0]) >= 6, "shield path is multi-segment");
    }

    #[test]
    fn icon_wifi_parses_to_non_empty_bezpath() {
        let sub = icon_subpaths(Icon::Wifi);
        assert_eq!(sub.len(), 4, "wifi = 3 arcs + 1 dot path");
        for p in &sub {
            assert!(element_count(p) > 0);
        }
    }

    #[test]
    fn icon_chevron_up_parses_to_non_empty_bezpath() {
        let sub = icon_subpaths(Icon::ChevronUp);
        assert_eq!(sub.len(), 1);
        assert!(element_count(&sub[0]) >= 3, "chevron is M,L,L");
    }

    #[test]
    fn icon_plus_has_two_strokes() {
        let sub = icon_subpaths(Icon::Plus);
        assert_eq!(sub.len(), 2, "plus = horizontal + vertical stroke");
    }

    #[test]
    fn icon_search_parses_to_non_empty_bezpath() {
        // Lucide's `search` glyph = one circle (magnifier body) + one
        // diagonal line (handle). Both subpaths must be non-empty so
        // the concept-parity swap in tabs/window-group doesn't ship
        // blank chrome if the enum is accidentally reverted.
        let sub = icon_subpaths(Icon::Search);
        assert_eq!(sub.len(), 2, "search = circle + handle line");
        for p in &sub {
            assert!(element_count(p) > 0, "search subpath must emit elements");
        }
    }

    #[test]
    fn draw_icon_does_not_panic_on_zero_rect() {
        // The guard in draw_icon makes zero-size rects a no-op rather
        // than a divide-by-zero. Cheap regression check.
        let mut scene = Scene::new();
        draw_icon(
            &mut scene,
            Affine::IDENTITY,
            Rect::new(0.0, 0.0, 0.0, 0.0),
            Icon::LayoutGrid,
            Color::WHITE,
            LUCIDE_DEFAULT_STROKE_PX,
        );
    }

    #[test]
    fn all_icons_produce_non_empty_subpaths() {
        // Smoke test over the whole enum so a typo in a new d="…"
        // literal is caught at `cargo test` rather than shipping as a
        // blank square in the shell.
        for icon in [
            Icon::LayoutGrid,
            Icon::Settings2,
            Icon::Wifi,
            Icon::Monitor,
            Icon::Battery,
            Icon::Shield,
            Icon::Maximize2,
            Icon::Minimize2,
            Icon::X,
            Icon::ChevronUp,
            Icon::ChevronDown,
            Icon::Plus,
            Icon::Search,
        ] {
            let sub = icon_subpaths(icon);
            assert!(!sub.is_empty(), "{icon:?} produced no subpaths");
            for p in &sub {
                assert!(element_count(p) > 0, "{icon:?} emitted empty subpath");
            }
        }
    }
}
