//! The hint that names the way out of a fullscreen session, shown as it
//! begins and gone once its time is up.

use vello::Scene;
use vello::kurbo::{Affine, Rect};

use ui::text_render::TextRenderer;
use ui::theme::Theme;

/// How long the way out of full screen is said over the session.
const FULLSCREEN_HINT: std::time::Duration = std::time::Duration::from_secs(3);

use super::RemoteApp;

impl RemoteApp {
    /// When the way out of full screen stops being said, while it is.
    pub(super) fn fullscreen_hint_deadline(&self) -> Option<ui::time::Instant> {
        self.fullscreen_hint
            .filter(|_| self.fullscreen)
            .and_then(|since| since.checked_add(FULLSCREEN_HINT))
            .filter(|due| ui::time::wall_frozen() || *due > ui::time::now())
    }

    /// "Press Esc to leave full screen." on the floating material at the
    /// top of the session, for [`FULLSCREEN_HINT`] after it went full
    /// screen.
    pub(super) fn draw_fullscreen_hint(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        if self.fullscreen_hint_deadline().is_none() {
            self.fullscreen_hint = None;
            return;
        }
        let text = "Press Esc to leave full screen.";
        let style = theme.text(ui::tokens::TextRole::Body);
        let (width, height) = text_renderer.measure_style(text, None, &style);
        let pad_x = f64::from(theme.tokens.spacing.s4);
        let pad_y = f64::from(theme.tokens.spacing.s2);
        let gap = f64::from(theme.tokens.spacing.s6);
        let w = f64::from(width) + 2.0 * pad_x;
        let h = f64::from(height) + 2.0 * pad_y;
        let x0 = self.rect.center().x - w / 2.0;
        let pill = Rect::new(x0, self.rect.y0 + gap, x0 + w, self.rect.y0 + gap + h);
        ui::widget::floating_surface::paint_card(scene, transform, pill, theme);
        text_renderer.draw_style(
            scene,
            text,
            theme.tokens.colors.text_primary,
            transform * Affine::translate((pill.x0 + pad_x, pill.y0 + pad_y)),
            None,
            &style,
        );
    }
}
