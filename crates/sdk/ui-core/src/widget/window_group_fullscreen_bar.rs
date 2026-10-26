//! A fullscreen window's bar: hidden while the window fills the screen,
//! drawn over its content when the person reaches for it, so the content
//! keeps its whole size and nothing moves.

use taffy::{Dimension, NodeId, TaffyTree};
use vello::Scene;
use vello::kurbo::Affine;

use super::WindowGroup;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::Widget;

/// A fullscreen window's bar as the person reached for it.
#[derive(Default)]
pub(super) struct FullscreenBar {
    /// Shown over the window: drawn over the content, which keeps its whole
    /// size.
    pub(super) revealed: bool,
    /// Its fade (`motion::CROSS_FADE`); reduced motion cuts.
    pub(super) fade: crate::motion::Presence,
    /// The last layout stood it over the content; once its fade-out ends
    /// the next one drops it, so nothing takes input where it was.
    pub(super) laid_out: bool,
}

impl WindowGroup {
    /// Whether a fullscreen window's bar is drawn: shown, or still fading
    /// out.
    pub(super) fn fullscreen_bar_drawn(&self) -> bool {
        self.fullscreen_bar.revealed || self.fullscreen_bar.fade.is_visible(crate::time::now())
    }

    /// The bar stands over the content rather than above it: a fullscreen
    /// window's bar the person reached for.
    pub(super) fn bar_over_content(&self) -> bool {
        self.fullscreen_presentation && self.fullscreen_bar_drawn() && !self.external_navigation
    }

    /// Whether the window's bar is in use (its location field, its search,
    /// a tab being dragged), so a fullscreen window's bar stays.
    #[must_use]
    pub fn bar_in_use(&self) -> bool {
        self.editing_location() || self.search.is_open() || self.drag_in_flight()
    }

    /// Whether a fullscreen window's bar shows over it now.
    #[must_use]
    pub fn fullscreen_bar_shown(&self) -> bool {
        self.fullscreen_presentation && self.fullscreen_bar.revealed && !self.external_navigation
    }

    /// The bar and the window's search under it.
    pub(super) fn draw_bar(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.bar.set_maximized(self.maximized);
        self.bar
            .set_group_focused(self.focused || self.search_active);
        // The bar's menus stay inside the window.
        self.bar.set_popup_area(self.rect);
        self.bar.draw(scene, transform, tr, theme, presence);
        self.search.set_ground(self.bar.ground_color(theme));
        self.search.draw(scene, transform, tr, theme, presence);
    }

    /// Show or hide a fullscreen window's bar over it (the host reads the
    /// top edge for it, `top_edge_reveal`); nothing while not fullscreen.
    /// `source` is what showed or hid it: the pointer's reach fades it, a
    /// key or an action (entering fullscreen by Super+F, Super alone) lands
    /// it at once.
    pub fn set_fullscreen_bar_revealed(
        &mut self,
        revealed: bool,
        source: crate::tokens::MotionSource,
    ) {
        if self.fullscreen_bar.revealed != revealed {
            self.fullscreen_bar.revealed = revealed;
            self.style_dirty = true;
            // It fades in place, so nothing under it moves; reduced motion
            // cuts.
            if self.chrome_motion.reduced() {
                self.fullscreen_bar.fade.snap(revealed);
            } else {
                self.fullscreen_bar.fade.show(
                    revealed,
                    crate::motion::CROSS_FADE,
                    self.chrome_motion,
                    source,
                    crate::time::now(),
                );
            }
        }
    }

    /// Over a fullscreen window the bar takes no room of its own: the
    /// content keeps its whole size, and the bar stands over its top.
    pub(super) fn lay_bar_over_content(&self, taffy: &mut TaffyTree<()>, bar_node: Option<NodeId>) {
        if self.bar_over_content()
            && let Some(bar) = bar_node
            && let Ok(mut bar_style) = taffy.style(bar).cloned()
        {
            bar_style.position = taffy::Position::Absolute;
            bar_style.inset.top = taffy::LengthPercentageAuto::length(0.0);
            bar_style.inset.left = taffy::LengthPercentageAuto::length(0.0);
            bar_style.size.width = Dimension::percent(1.0);
            let _ = taffy.set_style(bar, bar_style);
        }
    }

    /// Whether a fullscreen window's bar is fading in or out.
    pub(super) fn fullscreen_bar_fading(&self) -> bool {
        self.fullscreen_presentation && self.fullscreen_bar.fade.is_moving(crate::time::now())
    }

    /// A fullscreen window's bar standing over its content (`over`),
    /// fading with its reveal.
    pub(super) fn draw_bar_over_content(
        &mut self,
        over: bool,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if !over {
            return;
        }
        let alpha =
            crate::motion::unit_opacity(self.fullscreen_bar.fade.progress(crate::time::now()));
        let fading = alpha < 1.0;
        if fading {
            scene.push_layer(
                vello::peniko::Fill::NonZero,
                vello::peniko::BlendMode::default(),
                alpha,
                transform,
                // The window's search hangs under the bar.
                &self.rect,
            );
        }
        // Over the content it stands on its own ground, so the app's top
        // row never shows through it.
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            self.bar.ground_color(theme),
            None,
            &self.bar.get_rect(),
        );
        self.draw_bar(scene, transform, tr, theme, presence);
        if fading {
            scene.pop_layer();
        }
    }
}
