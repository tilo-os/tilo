//! What the editor root paints: the sidebar panel with its header, and the
//! content column's rows (tabs, find bar, editor, bottom panel, status bar).
//!
//! Nothing here fills a ground. The window ground and each region's tier are
//! the framework's: the sidebar stands on `Region::Sidebar` (painted by
//! [`SidebarDrawer::paint_panel`]), everything else on the content ground.

use ui::icons::Icon;
use ui::paint::PaintCtx;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::widget::{EmptyState, EmptyStateContent, Widget};
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

        // The toggle leads the header, where `NavShell` puts it.
        self.toggle_rect = Self::toggle_rect_in(header, theme);
        self.drawer.draw_toggle(
            scene,
            transform,
            text_renderer,
            &local,
            self.toggle_rect,
            self.toggle_hovered,
        );

        // The project's name, beside it.
        let gap = f64::from(local.tokens.spacing.s2);
        let x = self.toggle_rect.x1 + gap;
        let room = (header.x1 - f64::from(local.tokens.spacing.s3) - x).max(0.0);
        let name = text_renderer.elide_to_width_style(
            &self.session.project.name,
            room as f32,
            &local.text(TextRole::Subheading),
        );
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
        self.sidebar_empty_action = Rect::ZERO;
        if self.session.project.nodes.len() <= 1 {
            self.sidebar_empty_action = EmptyState::paint(
                &mut PaintCtx::new(scene, transform, text_renderer, &local),
                body,
                &EmptyStateContent {
                    icon: Some(Icon::FolderOpen),
                    action_label: Some("Open folder"),
                    ..EmptyStateContent::new("No folder open")
                },
                self.last_cursor,
            )
            .unwrap_or(Rect::ZERO);
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
        self.tabs
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
