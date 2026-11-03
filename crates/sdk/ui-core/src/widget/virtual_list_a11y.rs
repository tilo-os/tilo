//! The accessibility tree of a [`super::VirtualList`]: its mounted rows, and the heading
//! standing before each group of them.
//!
//! Each heading is a disclosure of its own, as the band over its group is: focusable, named
//! after the group, expanded while its rows show, and carrying the number of rows it holds,
//! so a reader hears what folding it away would hide. An action on such a node comes back to
//! the list by its [`super::groups::HEADING_TARGET`].

use super::groups::{Band, Group, HEADING_TARGET};
use super::{ListModel, MountedRow, MountedRowState, VirtualList};
use crate::a11y::{AccessNode, AccessState, Role};
use crate::widget::Widget;
use vello::kurbo::Rect;

impl<M: ListModel> VirtualList<M> {
    /// The node of one mounted row: a list item holding the row's own semantics.
    pub(super) fn row_node(
        &self,
        row: &MountedRow<M::Key>,
        enabled: bool,
    ) -> crate::a11y::AccessNode {
        let count = self.model.len();
        let child = row.widget.accessibility();
        let name = child
            .as_ref()
            .and_then(|node| node.name.clone())
            .or_else(|| first_descendant_accessible_name(row.widget.as_ref()));
        let mut item = AccessNode::new(Role::ListItem)
            .with_optional_name(name)
            .with_position(row.index.saturating_add(1), count)
            .with_state(
                AccessState::new()
                    .focusable(true)
                    .focused(self.is_focused && self.focused_key.as_ref() == Some(&row.key))
                    .selected(self.selection.is_selected(&row.key))
                    .disabled(!enabled),
            );
        match &row.state {
            MountedRowState::Ready => {}
            MountedRowState::Pending => item.state.busy = true,
            MountedRowState::Failed(message, _) => {
                item.state.invalid = true;
                item.description = Some(message.clone());
            }
        }
        if let Some(row_box) = self.row_box(row.index) {
            // The row's own box, below the band of a heading it starts a group under: one
            // bleed in from the list's edges.
            let widget = row.widget.get_rect();
            item = item.with_bounds(Rect::new(widget.x0, row_box.y0, widget.x1, row_box.y1));
        }
        if let Some(encode) = &self.semantic_key {
            item = item.with_action_target(format!("key:{}", encode(&row.key)));
        }
        if let Some(child) = child {
            item = item.with_children(vec![child]);
        }
        item
    }

    /// The accessibility children the list carries, in reading order: each group's heading
    /// before the rows it holds, those still on screen.
    pub(super) fn accessibility_children(&self, enabled: bool) -> Vec<AccessNode> {
        let mut children = Vec::new();
        let mut bands = self.tree_bands().into_iter().peekable();
        for row in &self.mounted {
            while bands.peek().is_some_and(|band| band.index <= row.index) {
                if let Some(band) = bands.next() {
                    children.push(self.heading_node(band, enabled));
                }
            }
            children.push(self.row_node(row, enabled));
        }
        for band in bands {
            children.push(self.heading_node(band, enabled));
        }
        children
    }

    /// How many headings the tree stands before the mounted row `child_index`: the index of
    /// the row's own node among the list's children.
    pub(super) fn headings_before(&self, child_index: usize) -> usize {
        let Some(row) = self.mounted.get(child_index) else {
            return 0;
        };
        self.tree_bands()
            .iter()
            .filter(|band| band.index <= row.index)
            .count()
    }

    /// The bands the last layout placed, in the order they head the rows.
    fn tree_bands(&self) -> Vec<&Band> {
        let mut bands: Vec<&Band> = self.groups.bands.iter().collect();
        bands.sort_by_key(|band| band.index);
        bands
    }

    /// The heading's node in the accessibility tree: a disclosure saying whether its group
    /// shows and how many rows it holds.
    fn heading_node(&self, band: &Band, enabled: bool) -> AccessNode {
        let focused = self
            .focused_group
            .as_ref()
            .is_some_and(|(heading, _)| *heading == band.heading);
        AccessNode::new(Role::Button)
            .with_name(band.heading.clone())
            .with_value(self.heading_rows(band).to_string())
            .with_action_target(format!("{HEADING_TARGET}{}", band.heading))
            .with_bounds(self.band_rect(band.top))
            .with_state(
                AccessState::new()
                    .focusable(true)
                    .focused(self.is_focused && focused)
                    .disabled(!enabled)
                    .expanded(!band.closed),
            )
    }

    /// How many rows the band's group holds, as the model says.
    pub(super) fn heading_rows(&self, band: &Band) -> usize {
        let len = self.model.len();
        let tail = self.tail_title.as_deref();
        let same = |index: usize| {
            super::groups::group_of(&self.model, index, tail)
                .is_some_and(|group: Group| group.0 == band.heading)
        };
        let mut start = band.index.min(len);
        while start > 0 && same(start - 1) {
            start -= 1;
        }
        let mut end = start;
        while end < len && same(end) {
            end += 1;
        }
        end - start
    }
}

fn first_descendant_accessible_name(widget: &dyn Widget) -> Option<String> {
    widget.children().into_iter().find_map(|child| {
        child
            .accessibility()
            .and_then(|node| node.name)
            .or_else(|| first_descendant_accessible_name(child))
    })
}
