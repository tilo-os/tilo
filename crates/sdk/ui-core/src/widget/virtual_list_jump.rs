//! The way back a list that follows its end offers: to the first unseen row while it stands
//! above the view, else to what is new, as a pill at the foot of the list.

use vello::kurbo::Rect;

use super::{ListModel, VirtualList};

impl<M: ListModel> VirtualList<M> {
    /// Follow the list's end (`VirtualListProps::follow_end`): it opens at its last row, and
    /// while it shows its end it stays there as rows arrive and settle their heights. A scroll
    /// away lets go; a scroll back to the end takes it up again.
    #[must_use]
    pub fn following_end(mut self) -> Self {
        self.follow_end = true;
        self.at_end = true;
        self.keep_end();
        self
    }

    /// Where the row the person has not seen begins (`VirtualListProps::unseen_from`): the
    /// divider stands over it, and while it is out of view a pill at the foot offers the way
    /// back to it ([`crate::widget::conversation::Jump`]), for a list that follows its end.
    /// `None` draws neither.
    #[must_use]
    pub fn unread_from(mut self, item: Option<String>) -> Self {
        self.unread = item;
        self
    }

    /// The item where the person's unseen part begins, as the list was told.
    #[must_use]
    pub fn unread_item(&self) -> Option<&str> {
        self.unread.as_deref()
    }

    /// The row showing the item the reader has not seen, as the list holds its rows now: the
    /// model's own answer, or, for a model of plain values (nothing a conversation holds), the
    /// keys the host gave.
    pub(super) fn unread_row(&self) -> Option<M::Key> {
        let item = self.unread.as_deref()?;
        self.model
            .key_presenting(item)
            .or_else(|| self.names_item.borrow().as_ref().and_then(|f| f(item)))
    }

    /// How the list turns the item its reader has not seen into the key of its row, for a model
    /// of plain values that names no resources: the host that renders rows knows them.
    pub fn set_key_presenting(&mut self, f: impl Fn(&str) -> Option<M::Key> + 'static) {
        *self.names_item.borrow_mut() = Some(Box::new(f));
    }

    /// Move the divider to the row `key`: the rows the old and the new one are on build again,
    /// so each shows the divider over it (or not) at once, without the list's items changing.
    pub fn set_unread(&mut self, item: Option<String>) {
        if self.unread == item {
            return;
        }
        let before = self.unread_row();
        self.unread = item;
        let after = self.unread_row();
        // Only the rows mounted now are built again; one the divider moves to is built with
        // the divider when it scrolls in anyway.
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

    /// Which way back the pill offers, when it offers one: to the first unread row while it is
    /// above the view, else to the newest row while the reader is away from its end. A
    /// list with nothing unseen offers only the way to what is now.
    #[must_use]
    pub(super) fn jump(&self) -> Option<crate::widget::conversation::Jump> {
        use crate::widget::conversation::Jump;
        if !self.follow_end || self.model.is_empty() {
            return None;
        }
        if let Some(key) = self.unread_row() {
            let in_view = self.model.index_of(&key).is_some_and(|index| {
                // Its row's foot is below the top of the view: the reader has it, or has read
                // past it. `item_view_top` is where the row stands in the view already.
                self.viewport
                    .item_view_top(index)
                    .is_some_and(|top| top + self.viewport.item_height(index).unwrap_or(0.0) > 1.0)
            });
            return (!in_view).then_some(Jump::Unread);
        }
        (!self.at_end).then_some(Jump::Present)
    }

    /// Take the reader to the first unread row, if there is one: it comes into view, and the
    /// rows arriving after it no longer move the reader.
    pub(super) fn jump_to_unread(&mut self) {
        let Some(key) = self.unread_row() else {
            return;
        };
        self.at_end = false;
        crate::motion::note_source(crate::tokens::MotionSource::Pointer);
        self.reveal(&key);
        // The pill is a jump, not a scroll the hand made: the reader lands there now, and
        // reads from it, rather than watching the rows glide for a while.
        let landed = self
            .scroll
            .to
            .take_over(crate::motion::frame_now())
            .map_or_else(|| self.scroll_offset(), |to| to.y);
        self.set_scroll_offset(landed);
    }

    /// Take the reader back to the newest row, following what arrives from then on.
    pub(super) fn jump_to_present(&mut self) {
        crate::motion::note_source(crate::tokens::MotionSource::Pointer);
        self.at_end = true;
        let _ = self.scroll.to.take_over(crate::motion::frame_now());
        self.set_scroll_offset(self.viewport.max_offset());
    }

    /// Take the way back the pill offers.
    pub(super) fn follow_pill(&mut self) {
        match self.jump() {
            Some(crate::widget::conversation::Jump::Unread) => self.jump_to_unread(),
            Some(crate::widget::conversation::Jump::Present) => self.jump_to_present(),
            None => {}
        }
    }

    /// The pill's rectangle in `area`, when it shows one: against the trailing edge of the
    /// column the rows stand in, a gutter in and a control's height above the foot, as wide as
    /// its word at the caption size.
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

    /// The pill's rectangle as the last layout placed it, for the pointer and a press.
    pub(super) fn pill_rect(&self) -> Option<Rect> {
        self.pill_rect_for(self.rect).map(|(_, rect)| rect)
    }

    /// The pill as it draws: what it offers, where, and whether it is hovered.
    pub(super) fn jump_face(&self) -> Option<crate::widget::conversation::JumpFace> {
        let (jump, rect) = self.pill_rect_for(self.rect)?;
        Some(crate::widget::conversation::JumpFace::new(jump, rect).hovered(self.jump_hovered))
    }

    /// How far a list that follows its end stands its rows down while they are shorter than its
    /// box: a transcript's few messages sit at its foot, by the composer, where the next one
    /// arrives, and none of them moves when the list grows past its box.
    pub(super) fn end_slack(&self) -> f64 {
        if self.follow_end {
            (self.viewport.viewport() - self.viewport.content_extent()).max(0.0)
        } else {
            0.0
        }
    }

    /// Whether the list shows its end, for one that follows it.
    #[must_use]
    pub fn is_at_end(&self) -> bool {
        self.follow_end && self.at_end
    }
}
