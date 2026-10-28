//! A model's rows on their way, in a [`VirtualList`]: what each row's
//! slot holds while it arrives, and asking again for one that failed.

use std::cell::Cell;
use std::rc::Rc;

use super::{ListModel, MountedRowState, VirtualList};
use crate::time::Instant;
use crate::widget::arriving_rows::RowReserve;
use crate::widget::{Element, Showing};

impl<M: ListModel> VirtualList<M> {
    /// What row `index` (`key`) shows at `now`, its model's answer seen
    /// through the row's placeholder timing.
    pub(super) fn observe_row(
        &mut self,
        index: usize,
        key: &M::Key,
        now: Instant,
    ) -> MountedRowState {
        let state = self.model.row_state(index);
        match self.arrivals.observe(key, &state, Some(self.motion), now) {
            Showing::Placeholder => MountedRowState::Pending,
            Showing::Answer => MountedRowState::Ready,
            Showing::Failure => MountedRowState::Failed(
                state
                    .failure()
                    .map_or_else(String::new, |message| (*message).to_owned()),
                self.model.can_retry(index),
            ),
        }
    }

    /// The widget in row `index`'s slot while it is not ready: the reserved
    /// slot of a pending row (its placeholder paints over it), or a failed
    /// row's reason with a retry when the model offers one.
    pub(super) fn arriving_row(&self, index: usize, state: &MountedRowState) -> Element {
        let height = self
            .viewport
            .item_height(index)
            .unwrap_or_else(|| self.viewport.estimated_height());
        arriving_row(index, state, &self.retry_asked, height)
    }

    /// Ask the model again for the failed row whose retry was pressed, if
    /// one was: whether the list changed.
    pub(super) fn take_retry(&mut self) -> bool {
        let Some(index) = self.retry_asked.take() else {
            return false;
        };
        self.model.retry(index);
        self.request_dirty = true;
        self.reconcile_mounted();
        true
    }
}

impl<M: ListModel + 'static> VirtualList<M> {
    /// When the list next changes on its own: a pending row's placeholder
    /// steps on, or a mounted row asks.
    pub(super) fn arrivals_wake(&self) -> Option<Instant> {
        self.arrivals
            .next_wake_deadline(crate::time::now())
            .into_iter()
            .chain(super::super::defaults::next_wake_deadline(&|visit| {
                crate::widget::Widget::for_each_child(self, visit);
            }))
            .min()
    }
}

/// Row `index`'s slot while it is not ready, `height` tall: the reserved
/// slot of a pending row, or a failed row's reason with a retry that sets
/// `retry_asked` when the model offers one. Not generic over the model, so
/// the toolkit builds it once for every list.
fn arriving_row(
    index: usize,
    state: &MountedRowState,
    retry_asked: &Rc<Cell<Option<usize>>>,
    height: f64,
) -> Element {
    match state {
        MountedRowState::Failed(message, retries) => {
            let reason = crate::widget::Text::new(message).dimmed();
            if !retries {
                return Box::new(reason);
            }
            let asked = Rc::clone(retry_asked);
            Box::new(crate::row![
                reason,
                crate::widget::Button::new("arriving.retry", "arriving-retry")
                    .on_view(move || asked.set(Some(index))),
            ])
        }
        MountedRowState::Ready | MountedRowState::Pending => RowReserve::element(height),
    }
}
