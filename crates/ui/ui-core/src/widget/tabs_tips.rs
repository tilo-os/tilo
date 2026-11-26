//! What a button in the bar names: the tip the pointer rests on. A tab someone else opened shows
//! the chip naming them instead ([`TabBar::holder_chip`]), never both.

use vello::kurbo::{Point, Rect};

use super::{TabBar, new_tab_name};

impl TabBar {
    pub(super) fn tip_at(&self, point: Point) -> Option<(Rect, String)> {
        if self.active_toolbar().is_some()
            && let Some(tip) = self.toolbar_row.tooltip_at(point)
        {
            return Some(tip);
        }
        if self.active_toolbar().is_some()
            && let [tab] = self.tabs.as_slice()
            && let Some(&icon) = self.tab_rects.first()
            && icon.contains(point)
        {
            return (!self.names_its_opener(0)).then(|| (icon, tab.title.clone()));
        }
        if let Some(index) = self
            .tab_rects
            .iter()
            .position(|pill| self.tab_hit_rect(*pill).contains(point))
            && let Some(tab) = self.tabs.get(index)
            && !tab.title.is_empty()
        {
            return (!self.names_its_opener(index))
                .then(|| (self.tab_rects[index], tab.title.clone()));
        }
        if !self.window_controls {
            return None;
        }
        if self.new_tab_rect.area() > 0.0 && self.new_tab_rect.contains(point) {
            return Some((self.new_tab_rect, new_tab_name()));
        }
        if self.history_action.area() > 0.0 && self.history_action.contains(point) {
            return Some((
                self.history_action,
                tilo_ui_base::i18n::I18nManager::lookup_plain("history-mode-title"),
            ));
        }
        let control = self.controls.hit(point)?;
        let rect = match control {
            crate::widget::window_controls::WindowControl::Maximize => self.controls.maximize,
            crate::widget::window_controls::WindowControl::Close => self.controls.close,
            crate::widget::window_controls::WindowControl::Minimize => self.controls.minimize,
        };
        Some((
            rect,
            tilo_ui_base::i18n::I18nManager::lookup_plain(control.name_key(self.resize)),
        ))
    }
}
