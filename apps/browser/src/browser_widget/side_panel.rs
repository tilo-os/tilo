//! An extension's side panel (`chrome.sidePanel`) beside the page: at the
//! window's trailing edge with a rule between them, never over the page.
//! Where the window has no room for both, the panel shows in the page's
//! place until the person closes it, one at a time.

use ui::theme::Theme;
use ui::widget::{WebView, Widget};

use super::BrowserWidget;

impl BrowserWidget {
    /// Whether the window has room for an extension's side panel beside a
    /// readable page: below it the panel shows in the page's place, one at a
    /// time, until the person closes it.
    fn side_panel_docks(&self, theme: &Theme) -> bool {
        let room = ui::num::f32_of_f64(self.rect.width());
        room >= crate::extension_controller::side_panel_width(theme)
            + theme.tokens.metrics.reading_min_width
    }

    /// The extension whose side panel shows, and whether it docks: when
    /// either changes the window is composed again.
    pub(super) fn side_panel_key(&self, theme: &Theme) -> Option<(String, bool)> {
        let id = self.extensions.side_panel_shown()?;
        Some((id.to_owned(), self.side_panel_docks(theme)))
    }

    /// The page, named `page_name` for assistive technology, with the side
    /// panel an extension shows beside it at the trailing edge, a rule
    /// between them, never over it.
    pub(super) fn page_beside_side_panel(
        &self,
        page_name: String,
        theme: &Theme,
    ) -> Box<dyn Widget> {
        let page: Box<dyn Widget> = Box::new(
            WebView::new(self.engine.clone())
                .with_scale_factor(self.window_scale)
                .with_accessible_name(page_name)
                .with_test_id("browser.surface"),
        );
        let docks = self.side_panel_docks(theme);
        let width = if docks {
            crate::extension_controller::side_panel_width(theme)
        } else {
            ui::num::f32_of_f64(self.rect.width())
        };
        let Some(panel) = self.extensions.side_panel(width) else {
            return page;
        };
        let row = ui::widget::Row::new()
            .no_wrap()
            .with_gap(theme.tokens.spacing.s0)
            .with_fill_remaining_height();
        if !docks {
            return Box::new(row.push(panel));
        }
        Box::new(
            row.push(Box::new(
                ui::widget::Container::new()
                    .with_no_padding()
                    .with_flex_grow()
                    .filling()
                    .with_child(page),
            ))
            .push(Box::new(ui::widget::Divider::vertical()))
            .push(panel),
        )
    }
}
