//! Accessibility adapter for custom-painted or externally virtualized lists.
//!
//! Prefer [`VirtualList`](super::VirtualList) when the toolkit can own row
//! widgets. Domain canvases that already own virtualization use this builder
//! so stable identity, positions, state, and exact visible bounds are still a
//! single explicit contract rather than bespoke `AccessNode` assembly.

use vello::kurbo::Rect;

use crate::a11y::{AccessNode, AccessState, Role};

#[derive(Clone, Debug)]
pub struct ListSemanticRow {
    stable_key: String,
    actionable: bool,
    name: String,
    description: Option<String>,
    bounds: Rect,
    position: usize,
    state: AccessState,
    children: Vec<AccessNode>,
}

impl ListSemanticRow {
    pub fn new(
        target: impl Into<String>,
        name: impl Into<String>,
        bounds: Rect,
        position: usize,
    ) -> Self {
        Self {
            stable_key: target.into(),
            actionable: true,
            name: name.into(),
            description: None,
            bounds,
            position,
            state: AccessState::default(),
            children: Vec::new(),
        }
    }

    /// A read-only row that still needs durable identity across virtualized
    /// rebuilds (for example an hourly forecast or activity-log entry).
    pub fn static_row(
        stable_key: impl Into<String>,
        name: impl Into<String>,
        bounds: Rect,
        position: usize,
    ) -> Self {
        Self {
            stable_key: stable_key.into(),
            actionable: false,
            name: name.into(),
            description: None,
            bounds,
            position,
            state: AccessState::default(),
            children: Vec::new(),
        }
    }

    #[must_use]
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    #[must_use]
    pub fn selected(mut self, selected: bool) -> Self {
        self.state.selected = selected;
        self
    }

    #[must_use]
    pub fn focused(mut self, focused: bool) -> Self {
        self.state.focused = focused;
        self
    }

    #[must_use]
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.state.disabled = disabled;
        self
    }

    /// Attach controls that visually belong to this row, such as Copy,
    /// Reorder, or Delete actions. Keeping them under the row preserves list
    /// navigation while exposing every visible affordance.
    #[must_use]
    pub fn children(mut self, children: impl IntoIterator<Item = AccessNode>) -> Self {
        self.children = children.into_iter().collect();
        self
    }

    fn access_node(&self, total: usize, viewport: Rect) -> Option<AccessNode> {
        let visible_bounds = self.bounds.intersect(viewport);
        if visible_bounds.area() <= 0.0 {
            return None;
        }
        let mut node = AccessNode::new(Role::ListItem)
            .with_name(self.name.clone())
            .with_bounds(visible_bounds)
            .with_position(self.position, total)
            .with_state(AccessState {
                focusable: self.actionable && !self.state.disabled,
                ..self.state
            });
        node = if self.actionable {
            node.with_action_target(self.stable_key.clone())
        } else {
            node.with_semantic_key(&self.stable_key)
        };
        if let Some(description) = &self.description {
            node = node.with_description(description.clone());
        }
        node.children = self
            .children
            .iter()
            .filter_map(|child| {
                let Some(bounds) = child.bounds.map(crate::a11y::AccessBounds::to_rect) else {
                    return Some(child.clone());
                };
                let clipped = bounds.intersect(viewport);
                (clipped.area() > 0.0).then(|| {
                    let mut child = child.clone();
                    child.bounds = Some(clipped.into());
                    child
                })
            })
            .collect();
        Some(node)
    }
}

#[derive(Clone, Debug)]
pub struct VirtualListSemantics {
    name: String,
    bounds: Rect,
    total: usize,
    rows: Vec<ListSemanticRow>,
    description: Option<String>,
}

impl VirtualListSemantics {
    pub fn new(name: impl Into<String>, bounds: Rect, total: usize) -> Self {
        Self {
            name: name.into(),
            bounds,
            total,
            rows: Vec::new(),
            description: None,
        }
    }

    #[must_use]
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    pub fn push(&mut self, row: ListSemanticRow) {
        self.rows.push(row);
    }

    pub fn accessibility(&self) -> AccessNode {
        let mut node = AccessNode::new(Role::List)
            .with_name(self.name.clone())
            .with_bounds(self.bounds)
            .with_children(
                self.rows
                    .iter()
                    .filter_map(|row| row.access_node(self.total, self.bounds))
                    .collect(),
            );
        if let Some(description) = &self.description {
            node = node.with_description(description.clone());
        }
        node
    }

    pub fn row_rect(&self, visible_ordinal: usize) -> Option<Rect> {
        self.rows
            .get(visible_ordinal)
            .map(|row| row.bounds.intersect(self.bounds))
            .filter(|bounds| bounds.area() > 0.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_semantics_publish_stable_positioned_rows_with_context() {
        // Case `builder_publishes_stable_bounded_positioned_rows`.
        {
            let bounds = Rect::new(0.0, 0.0, 200.0, 300.0);
            let row = Rect::new(0.0, 80.0, 200.0, 120.0);
            let mut list = VirtualListSemantics::new("Saved places", bounds, 50);
            list.push(
                ListSemanticRow::new("place:durable-id", "Reykjavík", row, 18)
                    .description("Iceland, snow, 2°")
                    .selected(true)
                    .focused(true),
            );
            let node = list.accessibility();
            assert_eq!(node.role, Role::List);
            assert_eq!(node.children.len(), 1);
            let item = &node.children[0];
            assert_eq!(item.action_target.as_deref(), Some("place:durable-id"));
            assert_eq!(
                item.bounds.map(crate::a11y::AccessBounds::to_rect),
                Some(row)
            );
            assert_eq!(item.pos_in_set, Some(18));
            assert_eq!(item.set_size, Some(50));
            assert!(item.state.selected && item.state.focused && item.state.focusable);
            assert_eq!(list.row_rect(0), Some(row));
        }

        // Case `static_row_is_stable_but_not_actionable`.
        {
            let row = Rect::new(0.0, 0.0, 100.0, 40.0);
            let mut list = VirtualListSemantics::new("Forecast", row, 1);
            list.push(ListSemanticRow::static_row("hour:14", "14:00", row, 1));
            let node = list.accessibility();
            let item = &node.children[0];
            assert_eq!(item.semantic_key.as_deref(), Some("hour:14"));
            assert!(item.action_target.is_none());
            assert!(!item.state.focusable);
        }

        // Case `rich_rows_preserve_container_context_and_nested_controls`.
        {
            let bounds = Rect::new(10.0, 20.0, 310.0, 220.0);
            let row = Rect::new(10.0, 60.0, 310.0, 108.0);
            let controls = vec![
                AccessNode::new(Role::Button)
                    .with_name("Copy code")
                    .with_action_target("account.copy:durable-id"),
                AccessNode::new(Role::Button)
                    .with_name("Move up")
                    .with_action_target("account.move-up:durable-id"),
                AccessNode::new(Role::Button)
                    .with_name("Delete")
                    .with_action_target("account.delete:durable-id"),
            ];
            let mut list = VirtualListSemantics::new("Accounts", bounds, 3)
                .description("3 visible accounts. Arrow keys change the selection.");
            list.push(
                ListSemanticRow::new("account.select:durable-id", "Example", row, 2)
                    .selected(true)
                    .children(controls),
            );

            let node = list.accessibility();
            assert_eq!(
                node.description.as_deref(),
                Some("3 visible accounts. Arrow keys change the selection.")
            );
            let item = &node.children[0];
            assert_eq!(
                item.action_target.as_deref(),
                Some("account.select:durable-id")
            );
            assert_eq!(
                item.bounds.map(crate::a11y::AccessBounds::to_rect),
                Some(row)
            );
            assert_eq!((item.pos_in_set, item.set_size), (Some(2), Some(3)));
            assert!(item.state.selected && item.state.focusable);
            assert_eq!(item.children.len(), 3);
            assert_eq!(
                item.children
                    .iter()
                    .filter_map(|control| control.action_target.as_deref())
                    .collect::<Vec<_>>(),
                vec![
                    "account.copy:durable-id",
                    "account.move-up:durable-id",
                    "account.delete:durable-id"
                ]
            );
        }

        // Case `viewport_clips_rows_and_omits_fully_hidden_controls`.
        {
            let viewport = Rect::new(0.0, 0.0, 200.0, 100.0);
            let row = Rect::new(0.0, 70.0, 200.0, 150.0);
            let visible = AccessNode::new(Role::Button)
                .with_name("Visible action")
                .with_bounds(Rect::new(10.0, 80.0, 50.0, 120.0));
            let hidden = AccessNode::new(Role::Button)
                .with_name("Hidden action")
                .with_bounds(Rect::new(100.0, 110.0, 150.0, 145.0));
            let mut list = VirtualListSemantics::new("Accounts", viewport, 20);
            list.push(
                ListSemanticRow::new("row:18", "Partial", row, 18).children([visible, hidden]),
            );

            let node = list.accessibility();
            let item = &node.children[0];
            assert_eq!(
                item.bounds.map(crate::a11y::AccessBounds::to_rect),
                Some(Rect::new(0.0, 70.0, 200.0, 100.0))
            );
            assert_eq!(item.children.len(), 1);
            assert_eq!(item.children[0].name.as_deref(), Some("Visible action"));
            assert_eq!(
                item.children[0]
                    .bounds
                    .map(crate::a11y::AccessBounds::to_rect),
                Some(Rect::new(10.0, 80.0, 50.0, 100.0))
            );
            assert_eq!(list.row_rect(0), Some(Rect::new(0.0, 70.0, 200.0, 100.0)));
        }
    }
}
