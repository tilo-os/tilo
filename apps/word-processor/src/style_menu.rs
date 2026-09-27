//! Paragraph-style management menu.
//!
//! Adapts the shared [`ContextMenu`] into typed delete/duplicate outcomes. The
//! editor keeps document mutation policy; this component owns menu lifecycle,
//! row mapping, hit-testing, and drawing.

use vello::Scene;
use vello::kurbo::{Affine, Point};

use ui::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;
use ui::widget::context_menu::{ContextMenu, MenuItem};

use crate::i18n::t;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum StyleMenuAction {
    Delete(String),
    Duplicate(String),
}

#[derive(Default)]
pub(crate) struct StyleMenu {
    menu: ContextMenu,
    actions: Vec<StyleMenuAction>,
}

impl StyleMenu {
    pub(crate) fn is_open(&self) -> bool {
        self.menu.is_open()
    }

    pub(crate) fn open(&mut self, style_id: &str, point: Point) {
        let mut menu = ContextMenu::new();
        let mut actions = Vec::new();
        if !crate::styles::StyleSheet::is_builtin(style_id) {
            menu.push(MenuItem::action(t("style-menu-delete"), || {}));
            actions.push(StyleMenuAction::Delete(style_id.to_string()));
        }
        menu.push(MenuItem::action(t("style-menu-duplicate"), || {}));
        actions.push(StyleMenuAction::Duplicate(style_id.to_string()));
        menu.open_at([
            crate::num::f32_of_f64(point.x),
            crate::num::f32_of_f64(point.y),
        ]);
        self.menu = menu;
        self.actions = actions;
    }

    pub(crate) fn close(&mut self) {
        self.menu.close();
    }

    pub(crate) fn menu_mut(&mut self) -> &mut ContextMenu {
        &mut self.menu
    }

    pub(crate) fn take_selection(&mut self) -> Option<StyleMenuAction> {
        self.menu
            .take_selection()
            .and_then(|index| self.actions.get(index).cloned())
    }

    /// Consume a click while open. An outside click dismisses the menu; a row
    /// click returns its typed action and also dismisses it.
    pub(crate) fn click(&mut self, point: Point) -> Option<StyleMenuAction> {
        let selected = self
            .menu
            .item_at([point.x, point.y])
            .and_then(|index| self.actions.get(index).cloned());
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
        self.menu.set_text_scale(theme.text_scale);
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
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Custom and builtin styles offer the right actions, a click returns a typed action and closes, and an
    /// outside click dismisses without an action.
    #[test]
    fn the_style_menu_offers_the_right_actions_and_closes_cleanly() {
        // Phase: custom_and_builtin_styles_offer_the_right_actions.
        {
            let mut menu = StyleMenu::default();
            menu.open("custom-1", Point::new(10.0, 20.0));
            assert_eq!(
                menu.actions,
                vec![
                    StyleMenuAction::Delete("custom-1".into()),
                    StyleMenuAction::Duplicate("custom-1".into()),
                ]
            );

            menu.open("body", Point::new(10.0, 20.0));
            assert_eq!(
                menu.actions,
                vec![StyleMenuAction::Duplicate("body".into())]
            );
        }

        // Phase: click_returns_a_typed_action_and_closes.
        {
            let mut menu = StyleMenu::default();
            menu.open("custom-1", Point::new(10.0, 20.0));
            let first_row = Point::new(20.0, 27.0);
            assert_eq!(
                menu.click(first_row),
                Some(StyleMenuAction::Delete("custom-1".into()))
            );
            assert!(!menu.is_open());
        }

        // Phase: outside_click_dismisses_without_an_action.
        {
            let mut menu = StyleMenu::default();
            menu.open("body", Point::new(10.0, 20.0));
            assert_eq!(menu.click(Point::ZERO), None);
            assert!(!menu.is_open());
        }
    }
}
