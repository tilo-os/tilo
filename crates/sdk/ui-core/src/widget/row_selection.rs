//! The selection a list's rows paint, and the row that takes it.

use vello::kurbo::Rect;

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
/// row's place in it: the row's selection, whether it stands open into its item, and the step
/// its tree indents by.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RowBuild {
    /// How the row stands in the list's selection.
    pub selection: RowSelection,
    /// The row stands open into its item where it stands in the list: the item's document
    /// shows under it, and the row is drawn to hold it.
    pub expanded: bool,
    /// The gutter one depth of the list's tree indents a row's content by, as the theme draws
    /// it: a row that is a view of the app's own stands in by the same step its list rows do.
    pub nest: f32,
    /// The column the list keeps for the chevrons that fold its tree, in front of its rows'
    /// content and inside its own box (where it stands as a column's body): a row's content
    /// starts after it, and the list draws the chevrons there. Zero for a list whose rows have
    /// nothing to fold, and for one standing inside a page, where the step in front of the rows
    /// is already the page's.
    pub chevron_column: f32,
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

/// The box a row's own content takes, in the space its rect answers in: a list that paints its
/// rows' selection paints it around this box, as a row that paints its own does, rather than
/// across the space the row's layout keeps around what it shows.
///
/// A layout keeps the gutter its content stands in — a page centres its column inside one, a
/// lane keeps the room for the column a row's leading control takes — and none of it belongs to
/// the row a person chose.
pub trait RowContentBox {
    /// The box the content stands in, in the space of this widget's own rect.
    fn row_content_box(&self) -> Option<Rect>;
}

/// The box `widget` — or the first of its children — shows its content in: what a list asks a
/// row's view for before it paints the row's selection.
pub fn row_content_box(widget: &dyn Widget) -> Option<Rect> {
    use crate::widget::capability::capability;
    if let Some(declared) = capability::<dyn RowContentBox>(widget) {
        return declared.row_content_box();
    }
    widget.children().into_iter().find_map(row_content_box)
}

/// A selection lift a list painted around one of its rows, as it painted it: the box, and what
/// the row shows that the box stands around.
///
/// An audit holds the one to the other: a lift reaching past what the row shows draws the
/// person's choice across the space beside it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RowLift {
    /// The box painted.
    pub rect: Rect,
    /// What the row shows: the lift stands around this box, `bleed` past it on each side.
    pub content: Rect,
    /// How far the lift reaches past the row's content on each side.
    pub bleed: f64,
}

/// A list that paints its rows' selection, asked for through
/// [`capability`](crate::widget::capability::capability): the lifts it painted last frame, in
/// window space.
pub trait PaintedRowLifts {
    /// The selection lifts the list painted for its rows.
    fn painted_row_lifts(&self) -> Vec<RowLift>;
}
