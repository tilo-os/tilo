//! Who opened a tab's window, when someone else did: their face leads the pill, right before its
//! title, so the strip reads "who, then what". An agent wears its mark in the same place, and on the
//! active tab that mark is the control that says what the agent is doing.

use super::{ICON_GAP, PILL_PAD_X, Point, Rect, TabBar, TabFaceSpec, square_at};
use tilo_ui_base::paint::PaintCtx;

impl TabBar {
    /// Who opened tab `id`'s window, worn before its title; `None` for a window of the person's
    /// own. Returns whether that changed.
    pub fn set_author(&mut self, id: u64, author: Option<TabFaceSpec>) -> bool {
        if self.authors.get(&id) == author.as_ref() {
            return false;
        }
        self.touch();
        match author {
            Some(author) => {
                self.authors.insert(id, author);
            }
            None => {
                self.authors.remove(&id);
            }
        }
        // The face widens the pill's leading zone, so the pills lay out again.
        self.face_demand_stale = true;
        true
    }

    /// Who opened tab `id`'s window, as its pill wears them.
    #[must_use]
    pub fn author(&self, id: u64) -> Option<&TabFaceSpec> {
        self.authors.get(&id)
    }

    /// The room tab `id`'s author takes before its title: their face and the same gap an app's
    /// glyph keeps from the title. Nothing for a tab of the person's own, or a pinned one.
    pub(super) fn author_slot(&self, id: u64) -> f64 {
        if self.icon_only(id) || !self.authors.contains_key(&id) {
            0.0
        } else {
            self.face_metrics.face + ICON_GAP
        }
    }

    /// Where pill `i`, drawn at `r`, wears its author: after its app glyph, before its title.
    pub(super) fn author_rect_in(&self, i: usize, r: Rect) -> Option<Rect> {
        let tab = self.tabs.get(i)?;
        (self.author_slot(tab.id) > 0.0).then(|| {
            let face = self.face_metrics.face;
            let x0 = r.x0 + PILL_PAD_X + self.glyph_zone(i);
            square_at(Point::new(x0 + face / 2.0, r.center().y), face)
        })
    }

    /// Where tab `id` wears its author's face, as laid out.
    #[must_use]
    pub fn author_rect(&self, id: u64) -> Option<Rect> {
        let i = self.tabs.iter().position(|tab| tab.id == id)?;
        self.author_rect_in(i, *self.tab_rects.get(i)?)
    }

    /// Paint pill `i`'s author at `r`, ringed in what the face stands on.
    pub(super) fn draw_pill_author(
        &self,
        ctx: &mut PaintCtx<'_>,
        (i, r): (usize, Rect),
        ring: vello::peniko::Color,
    ) {
        let Some((tab, rect)) = self.tabs.get(i).zip(self.author_rect_in(i, r)) else {
            return;
        };
        let Some(author) = self.authors.get(&tab.id) else {
            return;
        };
        let colour = author.rgb.map_or_else(
            || ctx.theme.tokens.colors.presence.hue(author.hue),
            |rgb| ctx.theme.tokens.colors.presence.of_rgb(rgb),
        );
        super::paint_tab_face(ctx, rect, &author.name, colour, author.agent, ring);
    }
}
