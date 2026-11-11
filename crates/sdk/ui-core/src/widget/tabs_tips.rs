//! What a button in the bar names: the tip the pointer rests on.

use vello::kurbo::{Point, Rect};

use super::{NEW_TAB, TabBar};

impl TabBar {
    /// Every button in the bar names its outcome: the slots' buttons and
    /// the window's controls.
    pub(super) fn tip_at(&self, point: Point) -> Option<(Rect, String)> {
        if self.active_toolbar().is_some()
            && let Some(tip) = self.toolbar_row.tooltip_at(point)
        {
            return Some(tip);
        }
        // A window of one tab with slots shows that tab as its icon alone:
        // the icon names the tab, as its accessible node does.
        if self.active_toolbar().is_some()
            && let [tab] = self.tabs.as_slice()
            && let Some(&icon) = self.tab_rects.first()
            && icon.contains(point)
        {
            return Some((icon, tab.title.clone()));
        }
        // A title is cut to fit its pill: the tip names the whole of it.
        if let Some(index) = self
            .tab_rects
            .iter()
            .position(|pill| self.tab_hit_rect(*pill).contains(point))
            && let Some(tab) = self.tabs.get(index)
            && !tab.title.is_empty()
        {
            return Some((self.tab_rects[index], tab.title.clone()));
        }
        if !self.window_controls {
            return None;
        }
        // The strip's own door: its glyph needs no words in the row, and the tip names it.
        if self.new_tab_rect.area() > 0.0 && self.new_tab_rect.contains(point) {
            return Some((self.new_tab_rect, NEW_TAB.to_owned()));
        }
        if self.history_action.area() > 0.0 && self.history_action.contains(point) {
            return Some((
                self.history_action,
                crate::i18n::I18nManager::lookup_plain("history-mode-title"),
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
            crate::i18n::I18nManager::lookup_plain(control.name_key(self.resize)),
        ))
    }
}
