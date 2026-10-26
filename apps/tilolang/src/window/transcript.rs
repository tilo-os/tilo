//! A conversation's transcript: the `list` a frame with a composer shows above it, as a
//! [`VirtualList`] that opens at its newest row and follows it.
//!
//! The rows are the view's own, rendered when the list shows them (`row(list, i)`) and forgotten
//! when they scroll away (`hide_rows`), as the list column's are; each is built into widgets by
//! a [`Reconciler`] of its own. A conversation of ten thousand messages costs the messages on
//! screen. The view's changes reach the transcript as they are: a message inserted, removed or
//! moved is that change of the model, and one the view changed is read again and rebuilt alone.

use std::cell::RefCell;
use std::rc::Rc;

use tilolang_tilo::runtime::{UiNode, ViewInstance};
use ui::widget::arrange::{Page, PageRowEdges, Slot};
use ui::widget::{Element, Spacer, VirtualList};

use super::list::{RowRead, Rows};
use crate::reconcile::Reconciler;

/// What a message is estimated to take before it is measured.
const ESTIMATED_ROW: f32 = 64.0;

/// The rows of a transcript's list: as the view renders them.
pub struct ThreadSource {
    pub view: Rc<RefCell<ViewInstance>>,
    /// The key of the `list` node.
    pub list: String,
}

impl RowRead for ThreadSource {
    type Data = UiNode;

    fn view(&self) -> &Rc<RefCell<ViewInstance>> {
        &self.view
    }

    fn list(&self) -> &str {
        &self.list
    }

    fn data(&self, _view: &ViewInstance, row: &UiNode) -> Option<UiNode> {
        Some(row.clone())
    }

    fn label(_data: &UiNode) -> Option<String> {
        None
    }
}

/// The list a transcript shows its rows in.
pub type Thread = VirtualList<Rows<ThreadSource>>;

/// The transcript of `source`'s list, its rows built by `reconciler`, each in the column of the
/// page the composer is in: the rows span the list, so their content shares the composer's
/// edge, and the page's top and bottom spacing stand above the first row and below the last.
pub fn thread(source: Rc<ThreadSource>, mut reconciler: Reconciler) -> Thread {
    VirtualList::new(Rows::of(source), ESTIMATED_ROW, move |rows, _, key| {
        rows.find(key).map_or_else(
            || Box::new(Spacer::fixed(0.0)) as Element,
            |row| {
                Box::new(
                    Page::full()
                        .row()
                        .push_slot(Slot::new(reconciler.build(&row))),
                )
            },
        )
    })
    .flush()
    .with_edges(PageRowEdges::new())
    .filling()
    .following_end()
    .with_id("tilolang.transcript")
}

/// The `list` a frame with a composer shows as its transcript: its content when the content is
/// that one list.
pub fn thread_list(frame: &UiNode) -> Option<&UiNode> {
    frame
        .children
        .iter()
        .find(|child| child.kind == "composer")?;
    let mut content = frame
        .children
        .iter()
        .filter(|child| !matches!(child.kind.as_str(), "sidebar" | "inspector" | "composer"));
    let only = content.next().filter(|child| child.kind == "list")?;
    content.next().is_none().then_some(only)
}
