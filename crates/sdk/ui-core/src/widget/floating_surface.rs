//! The one body of every floating Panel: popovers, menus, modal sheets and
//! toasts.
//!
//! A floating Panel is the panel radius (`radii.panel`), the blurred
//! material with its opaque fallback, the derived panel edge plus the 1 px
//! inner light edge on a dark material, and one soft shadow
//! (`elevation.floating`). The overlays differ in placement and
//! interaction; they never differ in this.

use vello::Scene;
use vello::kurbo::{Affine, Rect};
use vello::peniko::{BlendMode, Color, Fill};

use crate::theme::Theme;

/// The corner radius of a floating Panel.
#[must_use]
pub fn radius(theme: &Theme) -> f64 {
    f64::from(theme.tokens.radii.panel)
}

/// Paint a floating Panel's body in `rect`: shadow, material, edge. Content
/// is drawn on top by the caller.
pub fn paint(scene: &mut Scene, transform: Affine, rect: Rect, theme: &Theme) {
    let radius = radius(theme);
    crate::surface::shadow(
        scene,
        transform,
        rect,
        radius,
        &theme.tokens.elevation.floating,
    );
    crate::surface::panel(scene, transform, rect, radius, theme);
}

/// [`paint`] at `alpha` (0..=1), for a Panel fading in or out. Pair with
/// drawing the content inside the same alpha.
pub fn paint_faded(scene: &mut Scene, transform: Affine, rect: Rect, alpha: f32, theme: &Theme) {
    let alpha = alpha.clamp(0.0, 1.0);
    if alpha <= 0.0 {
        return;
    }
    if alpha >= 1.0 {
        paint(scene, transform, rect, theme);
        return;
    }
    // The layer bounds include the shadow's reach so its fade is not cut.
    let reach = f64::from(theme.tokens.elevation.floating.blur) * 2.0
        + f64::from(theme.tokens.elevation.floating.offset_y).abs();
    scene.push_layer(
        Fill::NonZero,
        BlendMode::default(),
        alpha,
        transform,
        &rect.inflate(reach, reach),
    );
    paint(scene, transform, rect, theme);
    scene.pop_layer();
}

/// How far a menu's rows sit inside its Panel, on every side: the `s2`
/// step. Popover menus, dropdown lists and context menus share it, so the
/// armed row's corners run concentric with the panel in all of them.
#[must_use]
pub fn menu_inset(theme: &Theme) -> f64 {
    f64::from(theme.tokens.spacing.s2)
}

/// How many surface steps the armed row of a menu stands off its panel.
///
/// A selected row is a lift (`docs/design-language.md`, "No brand accent";
/// HIG §9.3). On a floating panel one step measured ΔL* ≈ 11 and read as
/// "no highlighting" on a dark desktop, so a panel takes a stronger lift
/// step: half a step more, which clears ΔL* 15 on every material
/// (`tests/menu_highlight_render.rs`) while staying a quiet grey on white,
/// like the sidebar lift.
pub const OVERLAY_LIFT: f32 = 1.5;

/// The fill of a menu's armed row (the row Enter or a click activates) and
/// the ink on it: the panel ground's overlay lift ([`OVERLAY_LIFT`]) with
/// primary ink measured on it. Hue-free under the default emphasis.
#[must_use]
pub fn armed_row(theme: &Theme) -> (Color, Color) {
    let colors = &theme.tokens.colors;
    let fill = colors.lift_on(crate::chrome::panel_ground(theme), OVERLAY_LIFT);
    let ink = colors.ink_on_any(
        &[fill],
        colors.text_primary,
        crate::tokens::colors::ON_FILL_MIN_CONTRAST,
    );
    (fill, ink)
}

/// Paint the armed row of a menu: the overlay lift of [`armed_row`],
/// concentric inside the panel (HIG §9.3). `row` is the row already inset
/// by `inset` from the panel's edge ([`menu_inset`] for a menu built on
/// it). Returns the ink for the row's label and glyphs.
pub fn paint_armed_row(
    scene: &mut Scene,
    transform: Affine,
    row: Rect,
    inset: f64,
    theme: &Theme,
) -> Color {
    let radii = &theme.tokens.radii;
    let radius = f64::from(radii.concentric(radii.panel, crate::num::f32_of_f64(inset)))
        .min(row.height() / 2.0);
    let (fill, ink) = armed_row(theme);
    scene.fill(
        Fill::NonZero,
        transform,
        fill,
        None,
        &vello::kurbo::RoundedRect::from_rect(row, radius),
    );
    ink
}

/// Which side of its anchor a floating Panel opens on.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum PopoverSide {
    Above,
    #[default]
    Below,
    Left,
    Right,
}

impl PopoverSide {
    #[must_use]
    pub fn opposite(self) -> Self {
        match self {
            Self::Above => Self::Below,
            Self::Below => Self::Above,
            Self::Left => Self::Right,
            Self::Right => Self::Left,
        }
    }

    fn vertical(self) -> bool {
        matches!(self, Self::Above | Self::Below)
    }
}

/// The room a Panel has on `side` of `anchor`, `gap` away, inside
/// `viewport`: the most a menu opening there can grow before it scrolls.
#[must_use]
pub fn anchored_room(anchor: Rect, side: PopoverSide, gap: f64, viewport: Rect) -> f64 {
    let anchor = clamp_anchor(anchor, viewport);
    match side {
        PopoverSide::Below => viewport.y1 - anchor.y1 - gap,
        PopoverSide::Above => anchor.y0 - gap - viewport.y0,
        PopoverSide::Right => viewport.x1 - anchor.x1 - gap,
        PopoverSide::Left => anchor.x0 - gap - viewport.x0,
    }
    .max(0.0)
}

/// `anchor` moved inside `viewport`.
fn clamp_anchor(anchor: Rect, viewport: Rect) -> Rect {
    let x = |v: f64| v.clamp(viewport.x0, viewport.x1.max(viewport.x0));
    let y = |v: f64| v.clamp(viewport.y0, viewport.y1.max(viewport.y0));
    Rect::new(x(anchor.x0), y(anchor.y0), x(anchor.x1), y(anchor.y1))
}

/// Where a floating Panel of `size` goes against `anchor` (the control that
/// opened it, or a zero-size pointer position) inside `viewport`: on `side`
/// of the anchor, `gap` away, its leading edge on the anchor's leading
/// edge. It **flips** to the other side when `side` has no room and the
/// other side has more, flips its alignment to the anchor's trailing edge
/// when it would run past the viewport's trailing edge, and then **shifts**
/// along the anchor into the viewport. Returns the panel and the side it
/// opened on.
///
/// Every anchored floating Panel (context menus, popovers, dropdown lists)
/// places itself through here, so a menu always opens at its trigger.
#[must_use]
pub fn place_anchored(
    anchor: Rect,
    size: (f64, f64),
    side: PopoverSide,
    gap: f64,
    viewport: Rect,
) -> (Rect, PopoverSide) {
    let (w, h) = (size.0.max(0.0), size.1.max(0.0));
    // An anchor past the viewport's edge (a pointer outside it) opens the
    // panel from the nearest point on the edge.
    let anchor = clamp_anchor(anchor, viewport);
    let room = |side: PopoverSide| anchored_room(anchor, side, gap, viewport);
    let need = if side.vertical() { h } else { w };
    let side = if room(side) >= need || room(side) >= room(side.opposite()) {
        side
    } else {
        side.opposite()
    };
    // Cross axis: leading-aligned with the anchor, or trailing-aligned when
    // the leading alignment runs past the viewport and that has more room.
    let cross = |a0: f64, a1: f64, len: f64, v0: f64, v1: f64| {
        if a0 + len <= v1 || (a1 - len < v0 && v1 - a0 >= a1 - v0) {
            a0
        } else {
            a1 - len
        }
    };
    let (x0, y0) = match side {
        PopoverSide::Below => (
            cross(anchor.x0, anchor.x1, w, viewport.x0, viewport.x1),
            anchor.y1 + gap,
        ),
        PopoverSide::Above => (
            cross(anchor.x0, anchor.x1, w, viewport.x0, viewport.x1),
            anchor.y0 - gap - h,
        ),
        PopoverSide::Right => (
            anchor.x1 + gap,
            cross(anchor.y0, anchor.y1, h, viewport.y0, viewport.y1),
        ),
        PopoverSide::Left => (
            anchor.x0 - gap - w,
            cross(anchor.y0, anchor.y1, h, viewport.y0, viewport.y1),
        ),
    };
    // Shift into the viewport on the cross axis only: on the main axis the
    // panel never slides over its own anchor. A caller whose panel can be
    // taller than either side's room caps it with [`anchored_room`].
    let (x0, y0) = if side.vertical() {
        (x0.min(viewport.x1 - w).max(viewport.x0), y0)
    } else {
        (x0, y0.min(viewport.y1 - h).max(viewport.y0))
    };
    (Rect::new(x0, y0, x0 + w, y0 + h), side)
}

/// A Panel's content laid out in its own tree: a floating Panel is placed
/// against an anchor or the window, never by its parent's flexbox, so its
/// content has no parent tree to be laid out in. The tree is kept so events
/// reach the content with a valid node.
pub(crate) struct DetachedContent {
    pub taffy: taffy::TaffyTree<()>,
    pub node: taffy::NodeId,
}

impl DetachedContent {
    /// Lay `content` out in `available` with the production solver.
    pub(crate) fn lay_out(
        content: &mut dyn crate::widget::Widget,
        theme: &Theme,
        available: taffy::Size<taffy::AvailableSpace>,
        text_renderer: &mut crate::text_render::TextRenderer,
    ) -> Self {
        let mut taffy = taffy::TaffyTree::<()>::new();
        let mut nodes = std::collections::HashMap::new();
        let node = content.build_layout(&mut taffy, theme, &mut nodes);
        let mut bindings = crate::bound_dispatch::BoundNodeMap::new();
        // SAFETY: `nodes` describes the tree just built from `content`, which
        // the caller holds exclusively for the whole call; no pointer escapes.
        let _ = unsafe {
            bindings.rebuild(content, &taffy, node, &nodes);
            crate::widget::compute_widget_layout(
                &mut taffy,
                node,
                available,
                &nodes,
                &bindings,
                text_renderer,
            )
        };
        Self { taffy, node }
    }

    /// The laid-out size of the content's root.
    pub(crate) fn size(&self) -> (f64, f64) {
        self.taffy.layout(self.node).map_or((0.0, 0.0), |l| {
            (f64::from(l.size.width), f64::from(l.size.height))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// CIE L* of a colour, from the crate's WCAG luminance.
    fn lightness(c: Color) -> f64 {
        let y = crate::a11y::relative_luminance(c);
        let f = if y > 216.0 / 24389.0 {
            y.cbrt()
        } else {
            (841.0 / 108.0) * y + 4.0 / 29.0
        };
        116.0 * f - 16.0
    }

    #[test]
    fn the_armed_row_is_a_lift_that_clears_its_floor_on_every_material() {
        for name in Theme::ALL {
            let theme = Theme::named(name).expect("shipped preset resolves");
            let colors = &theme.tokens.colors;
            let ground = crate::chrome::panel_ground(&theme);
            let (fill, ink) = armed_row(&theme);
            let delta = (lightness(fill) - lightness(ground)).abs();
            // The armed row's floor: CIE ΔL* 15 off its panel.
            assert!(
                delta >= 15.0,
                "{name}: the armed row stands only ΔL* {delta:.1} off its panel"
            );
            // A lift, not an inverted slab: the list lift points the same
            // way and the overlay step is the stronger of the two.
            let list = colors.lift_on(ground, 1.0);
            let list_delta = (lightness(list) - lightness(ground)).abs();
            assert!(
                delta > list_delta,
                "{name}: the overlay lift ({delta:.1}) is stronger than a list lift ({list_delta:.1})"
            );
            assert_ne!(
                fill, colors.intent.inverted,
                "{name}: not the inverted fill"
            );
            assert!(
                crate::a11y::contrast_ratio(ink, fill)
                    >= crate::tokens::colors::ON_FILL_MIN_CONTRAST,
                "{name}: the armed row's label is legible on it"
            );
        }
    }

    const WINDOW: Rect = Rect::new(0.0, 0.0, 1280.0, 800.0);

    #[test]
    fn an_anchored_panel_opens_at_its_trigger_and_flips_and_shifts_at_the_edges() {
        // Below the trigger, leading edges aligned.
        let button = Rect::new(200.0, 16.0, 232.0, 48.0);
        let (rect, side) = place_anchored(button, (160.0, 240.0), PopoverSide::Below, 4.0, WINDOW);
        assert_eq!(side, PopoverSide::Below);
        assert_eq!((rect.x0, rect.y0), (200.0, 52.0));

        // A header button at the trailing edge: the menu's trailing edge
        // lines up with the button's instead of being pushed off it.
        let more = Rect::new(1088.0, 16.0, 1112.0, 40.0);
        let (rect, _) = place_anchored(more, (220.0, 240.0), PopoverSide::Below, 4.0, WINDOW);
        assert!(
            (rect.x1 - more.x1).abs() < 1e-9,
            "trailing-aligned with its trigger"
        );
        assert!(rect.y0 >= more.y1, "below its trigger, not over it");

        // No room below: flips above.
        let low = Rect::new(400.0, 740.0, 480.0, 772.0);
        let (rect, side) = place_anchored(low, (160.0, 240.0), PopoverSide::Below, 4.0, WINDOW);
        assert_eq!(side, PopoverSide::Above);
        assert!((rect.y1 - (low.y0 - 4.0)).abs() < 1e-9);

        // A pointer in the bottom-trailing corner: flips up and to the
        // pointer's leading side, and stays in the window.
        let point = Rect::new(1270.0, 790.0, 1270.0, 790.0);
        let (rect, _) = place_anchored(point, (200.0, 300.0), PopoverSide::Below, 0.0, WINDOW);
        assert!(rect.x0 >= WINDOW.x0 && rect.y0 >= WINDOW.y0);
        assert!(rect.x1 <= WINDOW.x1 && rect.y1 <= WINDOW.y1);
        assert!((rect.x1 - 1270.0).abs() < 1e-9 && (rect.y1 - 790.0).abs() < 1e-9);

        // Wider than the room either way: shifted into the window.
        let middle = Rect::new(600.0, 100.0, 640.0, 120.0);
        let (rect, _) = place_anchored(middle, (1270.0, 100.0), PopoverSide::Below, 4.0, WINDOW);
        assert!(rect.x0 >= WINDOW.x0 && rect.x1 <= WINDOW.x1);
    }
}
