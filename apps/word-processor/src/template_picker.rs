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
    templates: &'static [Template],
}

impl Default for TemplatePicker {
    fn default() -> Self {
        let mut menu = ContextMenu::new();
        for template in Template::all() {
            menu.push(MenuItem::action(template.display_title(), || {}));
        }
        Self {
            menu,
            templates: Template::all(),
        }
    }
}

impl TemplatePicker {
    pub(crate) fn is_open(&self) -> bool {
        self.menu.is_open()
    }

    pub(crate) fn open_at(&mut self, point: Point) {
        // Start each invocation without a stale hover or queued selection.
        *self = Self::default();
        self.menu.open_at([
            crate::num::f32_of_f64(point.x),
            crate::num::f32_of_f64(point.y),
        ]);
    }

    pub(crate) fn set_text_scale(&mut self, text_scale: f32) {
        self.menu.set_text_scale(text_scale);
    }

    pub(crate) fn set_viewport(&mut self, viewport: vello::kurbo::Rect) {
        self.menu.set_viewport(Some(viewport));
    }

    pub(crate) fn close(&mut self) {
        self.menu.close();
    }

    /// The shared menu owns pointer tracking and keyboard navigation.
    pub(crate) fn menu_mut(&mut self) -> &mut ContextMenu {
        &mut self.menu
    }

    pub(crate) fn take_selection(&mut self) -> Option<Template> {
        self.menu
            .take_selection()
            .and_then(|index| self.templates.get(index).copied())
    }

    /// Consume a click while open. A click outside the menu dismisses it;
    /// clicking a row returns its typed template and also closes the menu.
    pub(crate) fn click(&mut self, point: Point) -> Option<Template> {
        let selected = self
            .menu
            .item_at([point.x, point.y])
            .and_then(|index| self.templates.get(index).copied());
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

    #[cfg(test)]
    pub(crate) fn item_center(&self, template: Template) -> Option<Point> {
        let index = self.templates.iter().position(|item| *item == template)?;
        self.menu.item_rect(index).map(|rect| rect.center())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Rows follow the canonical template order, a click returns a typed template and closes, a scaled
    /// picker keeps rows mapped, and an outside click dismisses without a selection.
    #[test]
    fn the_template_picker_maps_rows_to_templates_at_every_scale() {
        // Phase: rows_follow_the_canonical_template_order.
        {
            let picker = TemplatePicker::default();
            assert_eq!(picker.templates, Template::all());
            assert_eq!(picker.labels().len(), picker.templates.len());
            assert!(picker.labels().iter().all(|label| !label.is_empty()));
        }

        // Phase: click_returns_a_typed_template_and_closes.
        {
            let mut picker = TemplatePicker::default();
            picker.open_at(Point::new(10.0, 20.0));
            let report_row = picker.item_center(Template::Report).unwrap();
            assert_eq!(picker.click(report_row), Some(Template::Report));
            assert!(!picker.is_open());
        }

        // Phase: scaled_picker_keeps_rows_mapped_to_their_templates.
        {
            let mut picker = TemplatePicker::default();
            picker.set_text_scale(2.0);
            picker.open_at(Point::new(10.0, 20.0));

            let report = picker
                .item_center(Template::Report)
                .expect("report stays an actionable template row at large text");
            assert_eq!(picker.click(report), Some(Template::Report));
        }

        // Phase: outside_click_dismisses_without_a_selection.
        {
            let mut picker = TemplatePicker::default();
            picker.open_at(Point::new(10.0, 20.0));
            assert_eq!(picker.click(Point::new(0.0, 0.0)), None);
            assert!(!picker.is_open());
        }
    }
}
