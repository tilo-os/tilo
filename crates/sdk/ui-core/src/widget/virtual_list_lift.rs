//! Where the boxes a list draws for its rows stand, and which rows take a lift: the strip a
//! selection paints, the band over a group and the seam a drop lands on all read the view's
//! tops, and a list that follows its end stands its rows down at its foot.

use super::ListModel;
use super::VirtualList;
use crate::widget::RowSelection;
use crate::widget::row_selection::selection_sink;
use vello::kurbo::Rect;

/// The rect a row's selection lift takes: around what the row shows, grown by the bleed a
/// [`ListRow`](crate::widget::ListRow) grows its own face by, and no further than the row's own
/// box — so a list that paints its rows' selection reads as one whose rows paint it, instead of
/// drawing the person's choice across the gutters the row's layout keeps.
pub(super) fn selection_around(row: Rect, content: Rect, bleed: f64) -> Rect {
    Rect::new(
        (content.x0 - bleed).max(row.x0),
        content.y0.clamp(row.y0, row.y1),
        (content.x1 + bleed).min(row.x1),
        content.y1.clamp(row.y0, row.y1),
    )
}

impl<M: ListModel> VirtualList<M> {
    /// Where a row standing `top` down the view (its top less the scroll) stands on screen. A
    /// list that follows its end stands a short list's rows down at its foot
    /// ([`VirtualList::end_slack`]) while the view keeps their tops from the top, so every box
    /// drawn for a row — its lift, its band, the seam a drop lands on — takes that slack with it.
    pub(super) fn shown_top(&self, top: f64) -> f64 {
        self.rect.y0 + self.end_slack() + top
    }

    /// The rect a row's lift takes: the list's width, less the bar's gutter
    /// while the rows stand beside the bar.
    pub(super) fn lift_rect(&self, top: f64, height: f64) -> Rect {
        let x1 = if self.rows_beside_bar {
            self.rect.x1 - self.gutter
        } else {
            self.rect.x1
        };
        let top = self.shown_top(top);
        Rect::new(self.rect.x0, top, x1, top + height)
    }

    pub(super) fn row_selection(&self, index: usize) -> RowSelection {
        let selected = |i: Option<usize>| {
            i.and_then(|i| self.model.key_at(i))
                .is_some_and(|key| self.selection.is_selected(&key))
        };
        RowSelection {
            selected: selected(Some(index)),
            above: selected(index.checked_sub(1)),
            below: selected(index.checked_add(1)),
        }
    }

    /// Hand every mounted row its current selection: a `ListRow` takes it
    /// in place, any other selection-aware delegate is rebuilt.
    pub(super) fn push_row_selection(&mut self) {
        if !self.delegate_selection && !self.selection_aware {
            return;
        }
        let selections: Vec<RowSelection> = self
            .mounted
            .iter()
            .map(|row| self.row_selection(row.index))
            .collect();
        let delegate = self.delegate_selection;
        let mut rebuild = false;
        for (row, selection) in self.mounted.iter_mut().zip(selections) {
            let sink = if delegate {
                selection_sink(&mut *row.widget)
            } else {
                None
            };
            if let Some(sink) = sink {
                sink.take_selection(selection);
                row.selection = selection;
            } else if row.selection != selection {
                rebuild = true;
            }
        }
        if rebuild {
            self.reconcile_mounted();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::selection_around;
    use vello::kurbo::Rect;

    /// The choice a list paints for a row stands around what the row shows, grown by the bleed
    /// the row grows its own face by and no further: a row standing at a page's measure inside a
    /// wider list paints its selection inset like the rows in every other list, never across the
    /// gutters beside it.
    #[test]
    fn a_lift_stands_around_what_its_row_shows_and_no_wider_than_the_row() {
        let row = Rect::new(624.0, 100.0, 1280.0, 148.0);
        let content = Rect::new(672.0, 100.0, 1232.0, 148.0);
        assert_eq!(
            selection_around(row, content, 8.0),
            Rect::new(664.0, 100.0, 1240.0, 148.0)
        );
        let whole = Rect::new(0.0, 0.0, 300.0, 40.0);
        assert_eq!(
            selection_around(whole, whole, 8.0),
            whole,
            "a row that shows everything it has keeps no inset"
        );
        let flush = Rect::new(10.0, 0.0, 300.0, 40.0);
        assert_eq!(
            selection_around(flush, flush, 8.0),
            flush,
            "a bleed reaches no further than the row's own box"
        );
        let past = Rect::new(20.0, 5.0, 280.0, 60.0);
        let row = Rect::new(10.0, 0.0, 300.0, 40.0);
        assert_eq!(
            selection_around(row, past, 8.0),
            Rect::new(12.0, 5.0, 288.0, 40.0),
            "a row showing past its box is kept inside it"
        );
    }
}
