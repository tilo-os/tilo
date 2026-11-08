//! The selection a list's rows paint, and the row that takes it.

use super::Widget;

/// A row's selection with its neighbours', for a delegate that paints its
/// own selection: a run of selected rows is one lift
/// ([`crate::chrome::RowJoin`]), with no rule between rows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RowSelection {
    pub selected: bool,
    pub above: bool,
    pub below: bool,
}

impl RowSelection {
    /// The lift's join with its selected neighbours.
    pub fn join(self) -> crate::chrome::RowJoin {
        crate::chrome::RowJoin::new(self.above, self.below)
    }
}

/// What a list tells its row delegate about the row it builds, besides the model and the
/// row's place in it: the row's selection, and whether it stands open into its item.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RowBuild {
    /// How the row stands in the list's selection.
    pub selection: RowSelection,
    /// The row stands open into its item where it stands in the list: the item's document
    /// shows under it, and the row is drawn to hold it.
    pub expanded: bool,
}

/// A row that paints the selection the list holds for it, asked for through
/// [`capability_mut`](crate::widget::capability::capability_mut): a view wraps the row it
/// places in its swipe and its hover toolbar, and the list reaches the row through them.
pub trait RowSelectionSink {
    /// Take the selection the list holds for this row.
    fn take_selection(&mut self, selection: RowSelection);
}

/// The row among `widget` and its children that takes a selection: a [`ListRow`] itself, the one
/// a view wrapped in its swipe or hover toolbar, or the one a mark stood beside.
///
/// [`ListRow`]: crate::widget::ListRow
pub(super) fn selection_sink<'a>(
    widget: &'a mut dyn Widget,
) -> Option<&'a mut (dyn RowSelectionSink + 'static)> {
    use crate::widget::capability::capability_mut;
    // Asked twice: the answer borrows `widget` for as long as it lives, and the walk below
    // needs it back.
    if capability_mut::<dyn RowSelectionSink>(widget).is_some() {
        return capability_mut::<dyn RowSelectionSink>(widget);
    }
    widget.children_mut().into_iter().find_map(selection_sink)
}
