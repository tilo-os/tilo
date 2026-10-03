//! Accessibility semantics for custom-painted, virtualized tables.
//!
//! Prefer [`super::DataTable`] when it can render the table. Canvas/immediate-
//! mode tables that cannot use it must still use this builder rather than
//! hand-assembling roles and keys: the builder makes stable global row ids,
//! explicit header/cell ids, collection positions, and focus state automatic.

use vello::kurbo::Rect;

use crate::a11y::{AccessNode, AccessState, Role};

fn clipped_visible(bounds: Rect, viewport: Rect) -> Option<Rect> {
    let clipped = bounds.intersect(viewport);
    (clipped.area() > 0.0).then_some(clipped)
}

#[derive(Clone, Debug, PartialEq)]
pub struct TableSemanticColumn {
    pub name: String,
    pub bounds: Option<Rect>,
    position: Option<(usize, usize)>,
}

impl TableSemanticColumn {
    pub fn new(name: impl Into<String>, bounds: Option<Rect>) -> Self {
        Self {
            name: name.into(),
            bounds,
            position: None,
        }
    }

    /// Zero-based absolute column index and full logical column count. Use for
    /// horizontally virtualized tables that export only a visible slice.
    #[must_use]
    pub fn position(mut self, index: usize, set_size: usize) -> Self {
        self.position = Some((index, set_size));
        self
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TableSemanticCell {
    pub value: Option<String>,
    pub bounds: Option<Rect>,
    pub selected: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TableSemanticControl {
    role: Role,
    name: String,
    target: String,
    bounds: Rect,
    state: AccessState,
}

impl TableSemanticControl {
    pub fn button(name: impl Into<String>, target: impl Into<String>, bounds: Rect) -> Self {
        Self {
            role: Role::Button,
            name: name.into(),
            target: target.into(),
            bounds,
            state: AccessState::new().focusable(true),
        }
    }

    pub fn toggle(
        name: impl Into<String>,
        target: impl Into<String>,
        bounds: Rect,
        checked: bool,
    ) -> Self {
        Self {
            role: Role::ToggleButton,
            name: name.into(),
            target: target.into(),
            bounds,
            state: AccessState::new().focusable(true).checked(checked),
        }
    }
}

impl TableSemanticCell {
    pub fn new(value: Option<String>, bounds: Option<Rect>) -> Self {
        Self {
            value,
            bounds,
            selected: false,
        }
    }

    #[must_use]
    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct TableSemanticRow {
    /// Zero-based absolute display index, never the index within the visible slice.
    pub index: usize,
    pub set_size: usize,
    pub bounds: Option<Rect>,
    pub cells: Vec<TableSemanticCell>,
    pub selected: bool,
    pub action_target: Option<String>,
    pub controls: Vec<TableSemanticControl>,
}

impl TableSemanticRow {
    pub fn new(
        index: usize,
        set_size: usize,
        bounds: Option<Rect>,
        cells: Vec<TableSemanticCell>,
    ) -> Self {
        Self {
            index,
            set_size,
            bounds,
            cells,
            selected: false,
            action_target: None,
            controls: Vec::new(),
        }
    }

    #[must_use]
    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    #[must_use]
    pub fn action_target(mut self, target: impl Into<String>) -> Self {
        self.action_target = Some(target.into());
        self
    }

    #[must_use]
    pub fn controls(mut self, controls: Vec<TableSemanticControl>) -> Self {
        self.controls = controls;
        self
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct VirtualTableSemantics {
    name: String,
    description: Option<String>,
    value: Option<String>,
    bounds: Rect,
    focused: bool,
    read_only: bool,
    key: Option<u64>,
    columns: Vec<TableSemanticColumn>,
    rows: Vec<TableSemanticRow>,
}

impl VirtualTableSemantics {
    pub fn new(name: impl Into<String>, bounds: Rect) -> Self {
        Self {
            name: name.into(),
            description: None,
            value: None,
            bounds,
            focused: false,
            read_only: true,
            key: None,
            columns: Vec::new(),
            rows: Vec::new(),
        }
    }

    #[must_use]
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    #[must_use]
    pub fn value(mut self, value: impl Into<String>) -> Self {
        self.value = Some(value.into());
        self
    }

    #[must_use]
    pub fn focused(mut self, focused: bool) -> Self {
        self.focused = focused;
        self
    }

    #[must_use]
    pub fn read_only(mut self, read_only: bool) -> Self {
        self.read_only = read_only;
        self
    }

    #[must_use]
    pub fn hashed_key(mut self, key: impl std::hash::Hash) -> Self {
        self.key = Some(crate::a11y::hash_key(&key));
        self
    }

    #[must_use]
    pub fn columns(mut self, columns: Vec<TableSemanticColumn>) -> Self {
        self.columns = columns;
        self
    }

    #[must_use]
    pub fn rows(mut self, rows: Vec<TableSemanticRow>) -> Self {
        self.rows = rows;
        self
    }

    pub fn build(self) -> AccessNode {
        let column_count = self.columns.len();
        let header_cells = self
            .columns
            .iter()
            .enumerate()
            .filter_map(|(index, column)| {
                let (logical_index, set_size) = column.position.unwrap_or((index, column_count));
                let mut node = AccessNode::new(Role::Cell)
                    .with_name(&column.name)
                    .with_key(logical_index as u64)
                    .with_position(logical_index.saturating_add(1), set_size);
                if let Some(bounds) = column.bounds {
                    let clipped = clipped_visible(bounds, self.bounds)?;
                    node = node.with_bounds(clipped);
                }
                Some(node)
            })
            .collect();
        let header = AccessNode::new(Role::Row)
            .with_name("Column headers")
            .with_key(0)
            .with_children(header_cells);

        let mut children = Vec::with_capacity(self.rows.len().saturating_add(1));
        children.push(header);
        children.extend(self.rows.into_iter().filter_map(|row| {
            let visible_row_bounds = match row.bounds {
                Some(bounds) => Some(clipped_visible(bounds, self.bounds)?),
                None => None,
            };
            let mut cells: Vec<AccessNode> = self
                .columns
                .iter()
                .enumerate()
                .filter_map(|(index, column)| {
                    let (logical_index, set_size) =
                        column.position.unwrap_or((index, column_count));
                    let cell = row.cells.get(index);
                    let mut node = AccessNode::new(Role::Cell)
                        .with_name(&column.name)
                        .with_key(logical_index as u64)
                        .with_optional_value(cell.and_then(|cell| cell.value.clone()))
                        .with_state(
                            AccessState::new().selected(cell.is_some_and(|cell| cell.selected)),
                        )
                        .with_position(logical_index.saturating_add(1), set_size);
                    if let Some(bounds) = cell.and_then(|cell| cell.bounds) {
                        let clipped = clipped_visible(bounds, self.bounds)?;
                        node = node.with_bounds(clipped);
                    }
                    Some(node)
                })
                .collect();
            cells.extend(row.controls.iter().filter_map(|control| {
                clipped_visible(control.bounds, self.bounds).map(|clipped| {
                    AccessNode::new(control.role)
                        .with_name(&control.name)
                        .with_hashed_key(&control.target)
                        .with_action_target(&control.target)
                        .with_bounds(clipped)
                        .with_state(control.state)
                })
            }));
            let actionable = row.action_target.is_some();
            let mut node = AccessNode::new(Role::Row)
                .with_name(format!("Row {}", row.index.saturating_add(1)))
                .with_key(row.index.saturating_add(1) as u64)
                .with_position(row.index.saturating_add(1), row.set_size)
                .with_state(
                    AccessState::new()
                        .focusable(actionable)
                        .selected(row.selected),
                )
                .with_children(cells);
            if let Some(target) = row.action_target {
                node = node.with_action_target(target);
            }
            if let Some(bounds) = visible_row_bounds {
                node = node.with_bounds(bounds);
            }
            Some(node)
        }));

        let mut table = AccessNode::new(Role::Table)
            .with_name(self.name)
            .with_state(
                AccessState::new()
                    .focusable(true)
                    .focused(self.focused)
                    .read_only(self.read_only),
            )
            .with_bounds(self.bounds)
            .with_children(children);
        table.key = self.key;
        table.description = self.description;
        table.value = self.value;
        table
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_semantics_enforce_keys_and_clip_to_the_viewport() {
        // Case `builder_enforces_global_row_and_explicit_cell_keys`.
        {
            let table = VirtualTableSemantics::new("Results", Rect::new(0.0, 0.0, 200.0, 100.0))
                .focused(true)
                .columns(vec![TableSemanticColumn::new("Name", None)])
                .rows(vec![
                    TableSemanticRow::new(
                        41,
                        100,
                        None,
                        vec![TableSemanticCell::new(Some("Ada".into()), None)],
                    )
                    .controls(vec![TableSemanticControl::toggle(
                        "Enable Ada",
                        "people.toggle:ada",
                        Rect::new(160.0, 40.0, 200.0, 70.0),
                        true,
                    )]),
                ])
                .build();
            assert!(table.state.focused);
            assert_eq!(table.children[0].key, Some(0));
            assert_eq!(table.children[0].children[0].key, Some(0));
            assert_eq!(table.children[1].key, Some(42));
            assert_eq!(table.children[1].pos_in_set, Some(42));
            assert_eq!(table.children[1].children[0].value.as_deref(), Some("Ada"));
            let toggle = &table.children[1].children[1];
            assert_eq!(toggle.role, Role::ToggleButton);
            assert_eq!(toggle.state.checked, Some(true));
            assert_eq!(toggle.action_target.as_deref(), Some("people.toggle:ada"));
        }

        // Case `viewport_clips_partial_geometry_and_omits_hidden_rows_and_controls`.
        {
            let viewport = Rect::new(0.0, 0.0, 200.0, 100.0);
            let table = VirtualTableSemantics::new("Results", viewport)
                .columns(vec![TableSemanticColumn::new(
                    "Name",
                    Some(Rect::new(0.0, -10.0, 150.0, 20.0)),
                )])
                .rows(vec![
                    TableSemanticRow::new(
                        8,
                        20,
                        Some(Rect::new(0.0, 70.0, 200.0, 130.0)),
                        vec![TableSemanticCell::new(
                            Some("Partial".into()),
                            Some(Rect::new(0.0, 70.0, 150.0, 130.0)),
                        )],
                    )
                    .controls(vec![
                        TableSemanticControl::button(
                            "Visible",
                            "row.visible",
                            Rect::new(150.0, 80.0, 190.0, 120.0),
                        ),
                        TableSemanticControl::button(
                            "Hidden",
                            "row.hidden",
                            Rect::new(150.0, 110.0, 190.0, 140.0),
                        ),
                    ]),
                    TableSemanticRow::new(9, 20, Some(Rect::new(0.0, 130.0, 200.0, 180.0)), vec![]),
                ])
                .build();

            assert_eq!(table.children.len(), 2, "header plus one visible row");
            let row = &table.children[1];
            assert_eq!(
                row.bounds.map(crate::a11y::AccessBounds::to_rect),
                Some(Rect::new(0.0, 70.0, 200.0, 100.0))
            );
            assert_eq!(row.children.len(), 2, "one cell and one visible control");
            assert_eq!(row.children[1].name.as_deref(), Some("Visible"));
            assert_eq!(
                row.children[1]
                    .bounds
                    .map(crate::a11y::AccessBounds::to_rect),
                Some(Rect::new(150.0, 80.0, 190.0, 100.0))
            );
        }
    }
}
