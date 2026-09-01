//! "New from template" menu for the word processor.
//!
//! The picker adapts the shared [`ContextMenu`] into a typed template outcome.
//! Document replacement stays with the editor, while menu lifecycle, rendering,
//! and the row-to-template mapping stay together here.

use vello::Scene;
use vello::kurbo::{Affine, Point};

use ui::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;
use ui::widget::context_menu::{ContextMenu, MenuItem};

use crate::Template;

/// Owns the template menu's lifecycle, drawing, hit-testing, and typed outcome.
pub(crate) struct TemplatePicker {
    menu: ContextMenu,
}

impl Default for TemplatePicker {
    fn default() -> Self {
        let mut menu = ContextMenu::new();
        for template in Template::all() {
            menu.push(MenuItem::action(template.display_title(), || {}));
        }
        Self { menu }
    }
}

impl TemplatePicker {
    pub(crate) fn is_open(&self) -> bool {
        self.menu.is_open()
    }

    pub(crate) fn open_at(&mut self, point: Point) {
        self.menu.open_at([
            crate::num::f32_of_f64(point.x),
            crate::num::f32_of_f64(point.y),
        ]);
    }

    pub(crate) fn close(&mut self) {
        self.menu.close();
    }

    /// Consume a click while open. A click outside the menu dismisses it;
    /// clicking a row returns its typed template and also closes the menu.
    pub(crate) fn click(&mut self, point: Point) -> Option<Template> {
        let selected = self
            .menu
            .item_at([point.x, point.y])
            .and_then(|index| Template::all().get(index).copied());
        self.menu.close();
        selected
    }

    pub(crate) fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if self.is_open() {
            Widget::draw(
                &mut self.menu,
                scene,
                transform,
                text_renderer,
                theme,
                presence,
            );
        }
    }

    #[cfg(test)]
    fn labels(&self) -> Vec<&str> {
        self.menu
            .items()
            .iter()
            .filter_map(MenuItem::label)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_follow_the_canonical_template_order() {
        let picker = TemplatePicker::default();
        let expected: Vec<String> = Template::all()
            .iter()
            .map(|template| template.display_title())
            .collect();
        assert_eq!(picker.labels(), expected);
    }

    #[test]
    fn click_returns_a_typed_template_and_closes() {
        let mut picker = TemplatePicker::default();
        picker.open_at(Point::new(10.0, 20.0));
        // ContextMenu rows begin after 6 px padding and are 28 px tall.
        let report_row = Point::new(20.0, 20.0 + 6.0 + 28.0 * 2.0 + 1.0);
        assert_eq!(picker.click(report_row), Some(Template::Report));
        assert!(!picker.is_open());
    }

    #[test]
    fn outside_click_dismisses_without_a_selection() {
        let mut picker = TemplatePicker::default();
        picker.open_at(Point::new(10.0, 20.0));
        assert_eq!(picker.click(Point::new(0.0, 0.0)), None);
        assert!(!picker.is_open());
    }
}
