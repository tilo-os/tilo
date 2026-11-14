//! The way back a list that follows its end offers: to the first unseen row while it stands
//! above the view, else to what is new, as a pill at the foot of the list.

use vello::kurbo::Rect;

use super::{ListModel, VirtualList};

/// What a press on the pill reaches the list by, as its accessible node names it: the way back is
/// one control wherever it stands, so a reader or an agent holds it across the rows moving.
pub(super) const PILL_TARGET: &str = "jump-pill";

impl<M: ListModel> VirtualList<M> {
    #[must_use]
    pub fn following_end(mut self) -> Self {
        self.follow_end = true;
        self.at_end = true;
        self.keep_end();
        self
    }

    #[must_use]
    pub fn unread_from(mut self, item: Option<String>) -> Self {
        self.unread = item;
        self
    }

    #[must_use]
    pub fn unread_item(&self) -> Option<&str> {
        self.unread.as_deref()
    }

    pub(super) fn unread_row(&self) -> Option<M::Key> {
        let item = self.unread.as_deref()?;
        self.model
            .key_presenting(item)
            .or_else(|| self.names_item.borrow().as_ref().and_then(|f| f(item)))
    }

    pub fn set_key_presenting(&mut self, f: impl Fn(&str) -> Option<M::Key> + 'static) {
        *self.names_item.borrow_mut() = Some(Box::new(f));
    }

    pub fn set_unread(&mut self, item: Option<String>) {
        if self.unread == item {
            return;
        }
        let before = self.unread_row();
        self.unread = item;
        let after = self.unread_row();
        for row in before.into_iter().chain(after) {
            if self.mounted.iter().any(|mounted| mounted.key == row) {
                self.invalidated.insert(row);
            }
        }
        if !self.invalidated.is_empty() {
            self.remount = true;
            self.layout_dirty = true;
        }
    }

    #[must_use]
    pub(super) fn jump(&self) -> Option<crate::widget::conversation::Jump> {
        use crate::widget::conversation::Jump;
        if !self.follow_end || self.model.is_empty() {
            return None;
        }
        if let Some(key) = self.unread_row() {
            let in_view = self.model.index_of(&key).is_some_and(|index| {
                self.viewport
                    .item_view_top(index)
                    .is_some_and(|top| top + self.viewport.item_height(index).unwrap_or(0.0) > 1.0)
            });
            return (!in_view).then_some(Jump::Unread);
        }
        (!self.at_end).then_some(Jump::Present)
    }

    pub(super) fn jump_to_unread(&mut self) {
        let Some(key) = self.unread_row() else {
            return;
        };
        self.at_end = false;
        crate::motion::note_source(crate::tokens::MotionSource::Pointer);
        self.reveal(&key);
        let landed = self
            .scroll
            .to
            .take_over(crate::motion::frame_now())
            .map_or_else(|| self.scroll_offset(), |to| to.y);
        self.set_scroll_offset(landed);
    }

    pub(super) fn jump_to_present(&mut self) {
        crate::motion::note_source(crate::tokens::MotionSource::Pointer);
        self.at_end = true;
        let _ = self.scroll.to.take_over(crate::motion::frame_now());
        self.set_scroll_offset(self.viewport.max_offset());
    }

    /// The pill becomes the control the keyboard or a reader is on: Return or Space takes it, as
    /// a press does. `false` while the list offers no way back.
    pub(super) fn focus_pill(&mut self) -> bool {
        if self.pill_rect().is_none() {
            return false;
        }
        self.focused_key = None;
        self.focused_group = None;
        self.jump_focused = true;
        true
    }

    /// Take the way back the pill offers, as pressing it does.
    pub(super) fn press_pill(&mut self) -> bool {
        if self.jump().is_none() {
            return false;
        }
        self.jump_focused = false;
        self.follow_pill();
        true
    }

    pub(super) fn follow_pill(&mut self) {
        match self.jump() {
            Some(crate::widget::conversation::Jump::Unread) => self.jump_to_unread(),
            Some(crate::widget::conversation::Jump::Present) => self.jump_to_present(),
            None => {}
        }
    }

    /// Where the pill stands in `area`: against the trailing edge, a gutter in, half a control's
    /// height above the foot, and as wide as the words it says.
    ///
    /// The pill floats over the rows it covers, so it never trades its words for a share of the
    /// width: a way back cut to "Jump to first unre…" names nothing at the window it is for. Only
    /// the box it stands in can bound it, and the words a way back is written in fit that at every
    /// width a window opens at.
    pub(super) fn pill_rect_for(
        &self,
        area: Rect,
    ) -> Option<(crate::widget::conversation::Jump, Rect)> {
        let jump = self.jump()?;
        let rect = crate::widget::conversation::pill_rect(
            area,
            self.gutter.max(self.bleed),
            self.pill_width(jump),
            self.pill_height,
        );
        Some((jump, rect))
    }

    /// The width the pill says `jump` in: the words measured at the face they are drawn in.
    pub(super) fn pill_width(&self, jump: crate::widget::conversation::Jump) -> f64 {
        self.pill_widths[jump.slot()]
    }

    pub(super) fn pill_rect(&self) -> Option<Rect> {
        self.pill_rect_for(self.rect).map(|(_, rect)| rect)
    }

    pub(super) fn jump_face(&self) -> Option<crate::widget::conversation::JumpFace> {
        let (jump, rect) = self.pill_rect_for(self.rect)?;
        Some(
            crate::widget::conversation::JumpFace::new(jump, rect)
                .hovered(self.jump_hovered)
                .focused(self.jump_focused),
        )
    }

    /// The way back the list offers over its rows, and whether the pill has room for its words:
    /// what a layout check reads to see a floating pill shows what it says.
    pub(super) fn pill_layout(&self) -> Option<crate::widget::ListPillLayout> {
        let (jump, rect) = self.pill_rect_for(self.rect)?;
        Some(crate::widget::ListPillLayout {
            needs: crate::num::f32_of_f64(self.pill_width(jump)),
            has: crate::num::f32_of_f64(rect.width()),
        })
    }

    pub(super) fn end_slack(&self) -> f64 {
        if self.follow_end {
            (self.viewport.viewport() - self.viewport.content_extent()).max(0.0)
        } else {
            0.0
        }
    }

    #[must_use]
    pub fn is_at_end(&self) -> bool {
        self.follow_end && self.at_end
    }
}
