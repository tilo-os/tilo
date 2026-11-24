//! A fullscreen window's bar: hidden while the window covers its display, and drawn over its
//! content while the person reaches for the shell's bar over it, so its name, leaving full screen
//! and closing are one press away. The content keeps its whole size, so nothing moves.

use taffy::{Dimension, NodeId, TaffyTree};
use vello::kurbo::Rect;

use super::WindowGroup;

use crate::widget::Widget as _;

impl WindowGroup {
    /// Whether the window covers its display and shows its bar over its content now.
    pub(super) fn bar_over_content(&self) -> bool {
        self.fullscreen_presentation && self.fullscreen_bar.is_some() && !self.external_navigation
    }

    /// Show the bar of a window covering its display over its content, `top` below its top edge,
    /// where the shell's bar shown over that edge ends; `None` hides it again. A window in its
    /// strip keeps the bar it always shows.
    pub fn set_fullscreen_bar(&mut self, top: Option<f64>) {
        if self.fullscreen_bar != top {
            self.fullscreen_bar = top;
            self.style_dirty = true;
        }
    }

    /// Where the bar stands over the window's content, as last laid out, while it does: the
    /// shell draws there over the window's own pixels.
    #[must_use]
    pub fn bar_over_content_rect(&self) -> Option<Rect> {
        self.bar_over_content().then(|| self.bar.bar_rect())
    }

    /// The window's own exit-full-screen control was pressed since the last call.
    pub fn take_pending_exit_fullscreen(&mut self) -> bool {
        std::mem::take(&mut self.pending_exit_fullscreen)
    }

    /// Over a fullscreen window the bar takes up nothing of its own: the content keeps its whole
    /// size, and the bar stands over its top.
    pub(super) fn lay_bar_over_content(&self, taffy: &mut TaffyTree<()>, bar_node: Option<NodeId>) {
        if self.bar_over_content()
            && let Some(top) = self.fullscreen_bar
            && let Some(bar) = bar_node
            && let Ok(mut bar_style) = taffy.style(bar).cloned()
        {
            bar_style.position = taffy::Position::Absolute;
            bar_style.inset.top = taffy::LengthPercentageAuto::length(crate::num::f32_of_f64(top));
            bar_style.inset.left = taffy::LengthPercentageAuto::length(0.0);
            bar_style.size.width = Dimension::percent(1.0);
            let _ = taffy.set_style(bar, bar_style);
        }
    }

    /// The bar over the content stands on its own ground, so the app's top row never shows
    /// through it.
    pub(super) fn draw_bar_over_content(&mut self, ctx: &mut crate::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, tr, theme, mut env) = ctx.parts();
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            self.bar.ground_color(theme),
            None,
            &self.bar.get_rect(),
        );
        self.draw_bar(&mut crate::draw::DrawCtx::new(
            scene,
            transform,
            tr,
            theme,
            env.clone(),
        ));
    }
}
