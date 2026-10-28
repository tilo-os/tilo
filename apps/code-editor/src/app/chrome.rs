//! What the editor root paints: the content column's rows (editor, bottom
//! panel, status bar). The shown file's name, the panel toggle and the search
//! are the window bar's.
//!
//! Nothing here fills a ground. The window ground and each region's tier are
//! the framework's.

use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;
use vello::Scene;
use vello::kurbo::Affine;

use super::CodeApp;

impl CodeApp {
    /// Paint the content column: the editor, the bottom panel and the status
    /// bar.
    pub(super) fn draw_content(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.editor
            .draw(scene, transform, text_renderer, theme, presence);
        if self.panel_visible {
            self.panel
                .draw(scene, transform, text_renderer, theme, presence);
        }
        let status = self.status_rect;
        if status.height() > 0.0 {
            self.draw_status(scene, transform, text_renderer, theme, status);
        }
    }
}
