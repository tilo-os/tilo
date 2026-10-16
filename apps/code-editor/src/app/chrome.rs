//! What the editor root paints: the sidebar panel with its header, and the
//! content column's rows (tabs, find bar, editor, bottom panel, status bar).
//!
//! Nothing here fills a ground. The window ground and each region's tier are
//! the framework's: the sidebar stands on `Region::Sidebar` (painted by
//! [`SidebarDrawer::paint_panel`]), everything else on the content ground.

use ui::paint::PaintCtx;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::widget::Widget;
use vello::Scene;
use vello::kurbo::{Affine, Rect};

use super::CodeApp;

impl CodeApp {
    /// Paint the sidebar panel: its region (docked) or floating material
    /// (drawer), the toggle and project name in its header, and the explorer
    /// under it.
    pub(super) fn draw_sidebar(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let local = self.drawer.paint_panel(scene, transform, theme);
        let (header, body) = self.sidebar_header_and_body(theme);

        // A docked column holds the toggle at its header's leading corner,
        // where the window's header row put it; a floating drawer opens under
        // that row, so its header carries the name alone.
        let gap = f64::from(local.tokens.spacing.s2);
        let x = if self.toggle_in_tab_strip() {
            header.x0 + f64::from(local.tokens.spacing.s4)
        } else {
            self.drawer
                .paint_toggle(scene, transform, text_renderer, &local);
            self.drawer.toggle_rect().x1 + gap
        };
        // The project's name, beside it; with no project there is none to
        // name.
        let room = (header.x1 - f64::from(local.tokens.spacing.s3) - x).max(0.0);
        let name = if self.has_project() {
            text_renderer.elide_to_width_style(
                &self.session.project.name,
                room as f32,
                &local.text(TextRole::Subheading),
            )
        } else {
            String::new()
        };
        PaintCtx::new(scene, transform, text_renderer, &local).text_role_in_band(
            &name,
            TextRole::Subheading,
            local.tokens.colors.text_primary,
            x,
            header,
            None,
        );

        // The explorer fills the body. Placing it each frame also follows a
        // drawer that is sliding in. With no project there is nothing to
        // list, and the body offers to open one.
        if !self.has_project() {
            self.sidebar_empty.apply_layout_external(body);
            self.sidebar_empty
                .draw(scene, transform, text_renderer, &local, presence);
            return;
        }
        if self.sidebar.get_rect() != body {
            self.sidebar.place_at(body);
        }
        self.sidebar
            .draw(scene, transform, text_renderer, &local, presence);
    }

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
        ui::chrome::HeaderRowFace::new(column, theme).paint_rule(scene, transform, theme);
        self.header
            .draw(scene, transform, text_renderer, theme, presence);
        self.search_button
            .draw(scene, transform, text_renderer, theme, presence);
        self.panel_toggle
            .draw(scene, transform, text_renderer, theme, presence);
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
