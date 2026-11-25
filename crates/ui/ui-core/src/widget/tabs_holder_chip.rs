//! The chip naming who opened a window someone else opened, in their colour, below its tab when
//! the pointer rests there. It stands in for the tab's own tip, never beside it.
use super::{Rect, TabBar, TextRenderer, Theme};
use crate::i18n::widget_format;
use vello::Scene;
use vello::kurbo::Affine;

impl TabBar {
    /// The chip naming whoever opened the tab under the pointer: its rect, inside `viewport`, and
    /// its words. It stands below the tab, where a tab's own tip would, so it never covers the
    /// bar's buttons, and it takes the place of that tip ([`Self::names_its_opener`]).
    pub(crate) fn holder_chip(
        &self,
        tr: &mut TextRenderer,
        theme: &Theme,
        viewport: Rect,
    ) -> Option<(Rect, String, (u8, u8, u8))> {
        if self.pending_drag.is_some() {
            return None;
        }
        let i = self.hover_idx?;
        let (t, rect) = self.tabs.get(i).zip(self.tab_rects.get(i))?;
        let holder = self.held_ids.get(&t.id)?;
        let label = widget_format(
            "tab-opened-by-person",
            &[("name", holder.name.clone().into())],
        );
        let at = crate::widget::tooltip::person_tooltip_rect_on(
            tr,
            theme,
            (*rect, crate::widget::TooltipSide::Below),
            &label,
            viewport,
        );
        Some((at, label, holder.rgb))
    }

    /// Whether the tab at `index` is one someone else opened: its chip names them when the pointer
    /// rests on it, so it shows no tip of its own as well.
    pub(super) fn names_its_opener(&self, index: usize) -> bool {
        self.tabs
            .get(index)
            .is_some_and(|tab| self.held_ids.contains_key(&tab.id))
    }

    /// Name whoever opened the tab under the pointer, in their colour ([`Self::holder_chip`]): on
    /// a window someone else keeps that also explains why it offers no close and will not move.
    pub(crate) fn draw_holder_chip(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        viewport: Rect,
    ) {
        let Some((rect, label, rgb)) = self.holder_chip(tr, theme, viewport) else {
            return;
        };
        let color = theme.tokens.colors.presence.of_rgb(rgb);
        crate::widget::tooltip::draw_chip_marked(
            scene,
            transform,
            tr,
            theme,
            rect,
            (&label, Some(color)),
        );
    }
}

#[cfg(test)]
#[path = "tabs_holder_chip_tests.rs"]
mod tests;
