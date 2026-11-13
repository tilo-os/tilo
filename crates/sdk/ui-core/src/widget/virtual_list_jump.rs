//! The way back a list that follows its end offers: to the first unseen row while it stands
//! above the view, else to what is new, as a pill at the foot of the list.

use vello::kurbo::Rect;

use super::{ListModel, VirtualList};

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

    pub(super) fn follow_pill(&mut self) {
        match self.jump() {
            Some(crate::widget::conversation::Jump::Unread) => self.jump_to_unread(),
            Some(crate::widget::conversation::Jump::Present) => self.jump_to_present(),
            None => {}
        }
    }

    pub(super) fn pill_rect_for(
        &self,
        area: Rect,
    ) -> Option<(crate::widget::conversation::Jump, Rect)> {
        let jump = self.jump()?;
        let gutter = self.gutter.max(self.bleed);
        let width = (area.width() * 0.5).min(self.pill_width);
        let height = self.pill_height;
        let rect = Rect::new(
            (area.x1 - gutter - width).max(area.x0),
            (area.y1 - gutter / 2.0 - height).max(area.y0),
            (area.x1 - gutter).max(area.x0),
            (area.y1 - gutter / 2.0).max(area.y0),
        );
        Some((jump, rect))
    }

    pub(super) fn pill_rect(&self) -> Option<Rect> {
        self.pill_rect_for(self.rect).map(|(_, rect)| rect)
    }

    pub(super) fn jump_face(&self) -> Option<crate::widget::conversation::JumpFace> {
        let (jump, rect) = self.pill_rect_for(self.rect)?;
        Some(crate::widget::conversation::JumpFace::new(jump, rect).hovered(self.jump_hovered))
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
