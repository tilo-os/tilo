//! The "New from template" menu: the shared [`ContextMenu`] with a typed outcome.
//! Replacing the document stays with the editor.

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

use ui::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;
use ui::widget::context_menu::{ContextMenu, MenuItem};

use crate::Template;

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

    pub(crate) fn open_anchored(&mut self, anchor: Rect, viewport: Rect) {
        *self = Self::default();
        self.menu.open_anchored(anchor, viewport);
    }

    pub(crate) fn set_text_scale(&mut self, text_scale: f32) {
        self.menu.set_text_scale(text_scale);
    }

    pub(crate) fn close(&mut self) {
        self.menu.close();
    }

    pub(crate) fn menu_mut(&mut self) -> &mut ContextMenu {
        &mut self.menu
    }

    pub(crate) fn take_selection(&mut self) -> Option<Template> {
        self.menu
            .take_selection()
            .and_then(|index| self.templates.get(index).copied())
    }

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
            Widget::draw_overlay(
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

    #[test]
    fn the_template_picker_maps_rows_to_templates_at_every_scale() {
        {
            let picker = TemplatePicker::default();
            assert_eq!(picker.templates, Template::all());
            assert_eq!(picker.labels().len(), picker.templates.len());
            assert!(picker.labels().iter().all(|label| !label.is_empty()));
        }

        {
            let mut picker = TemplatePicker::default();
            picker.open_anchored(
                Rect::new(10.0, 0.0, 60.0, 20.0),
                Rect::new(0.0, 0.0, 800.0, 600.0),
            );
            let report_row = picker.item_center(Template::Report).unwrap();
            assert_eq!(picker.click(report_row), Some(Template::Report));
            assert!(!picker.is_open());
        }

        {
            let mut picker = TemplatePicker::default();
            picker.set_text_scale(2.0);
            picker.open_anchored(
                Rect::new(10.0, 0.0, 60.0, 20.0),
                Rect::new(0.0, 0.0, 800.0, 600.0),
            );

            let report = picker
                .item_center(Template::Report)
                .expect("report stays an actionable template row at large text");
            assert_eq!(picker.click(report), Some(Template::Report));
        }

        {
            let mut picker = TemplatePicker::default();
            picker.open_anchored(
                Rect::new(10.0, 0.0, 60.0, 20.0),
                Rect::new(0.0, 0.0, 800.0, 600.0),
            );
            assert_eq!(picker.click(Point::new(0.0, 0.0)), None);
            assert!(!picker.is_open());
        }
    }
}
