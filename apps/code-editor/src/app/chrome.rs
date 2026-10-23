//! What the editor root paints: the content column's rows (file header, find
//! bar, editor, bottom panel, status bar).
//!
//! Nothing here fills a ground. The window ground and each region's tier are
//! the framework's.

use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;
use vello::Scene;
use vello::kurbo::{Affine, Rect};

use super::CodeApp;

impl CodeApp {
    /// Paint the content column: the tab strip with the panel toggle, the find
    /// bar, the editor, the bottom panel and the status bar.
    pub(super) fn draw_content(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        // The tab strip is the content column's header row: one hairline
        // parts it from the code, as under every other window's header.
        let column = Rect::new(
            self.editor.get_rect().x0,
            self.rect.y0,
            self.rect.x1,
            self.rect.y1,
        );
        if self.laid_out_header {
            ui::chrome::HeaderRowFace::new(column, theme).paint_rule(scene, transform, theme);
            self.header
                .draw(scene, transform, text_renderer, theme, presence);
            self.search_button
                .draw(scene, transform, text_renderer, theme, presence);
            self.panel_toggle
                .draw(scene, transform, text_renderer, theme, presence);
        }
        if self.find_open {
            self.find
                .draw(scene, transform, text_renderer, theme, presence);
        }
        self.editor
            .draw(scene, transform, text_renderer, theme, presence);
        if self.panel_visible {
            self.panel
                .draw(scene, transform, text_renderer, theme, presence);
        }
        let status = self.status_rect;
        if status.height() > 0.0 {
            let band = Rect::new(status.x0, status.y0, status.x1, status.y1);
            ui::chrome::status_bar::draw(
                scene,
                transform,
                text_renderer,
                theme,
                band,
                &self.status.left(),
                &self.status.right(),
            );
        }
    }
}
