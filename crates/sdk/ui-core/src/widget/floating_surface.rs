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
use vello::peniko::{BlendMode, Fill};

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

/// Paint the armed row of a menu, the row Enter or a click activates: an
/// inverted control concentric inside the panel (HIG §9.3). `row` is the
/// row already inset by [`menu_inset`]. Returns the ink for the row's label
/// and glyphs.
pub fn paint_armed_row(
    scene: &mut Scene,
    transform: Affine,
    row: Rect,
    theme: &Theme,
) -> vello::peniko::Color {
    let radii = &theme.tokens.radii;
    // `menu_inset` is the `s2` step; the radius follows it.
    let radius = f64::from(radii.concentric(radii.panel, theme.tokens.spacing.s2));
    let colors = &theme.tokens.colors;
    scene.fill(
        Fill::NonZero,
        transform,
        colors.intent.inverted,
        None,
        &vello::kurbo::RoundedRect::from_rect(row, radius),
    );
    colors.ink_on_any(
        &[colors.intent.inverted],
        colors.intent.on_inverted,
        crate::tokens::colors::ON_FILL_MIN_CONTRAST,
    )
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
