//! List-mode content. Uses Plan 2's `DataTable` primitive for rows with
//! columns (icon, name, size, modified). The `DataTable` in this workspace
//! is a typed generic over the row type — we use `Entry` directly and
//! register per-column cell renderers via `ColumnDef::with_render`.
//!
//! Sort UI is owned by the toolbar module; this builder just seeds a
//! `DataTable<Entry>` with columns that match the iced original
//! (icon | Name | Size | Modified).

use crate::model::Entry;
use ui::widget::data_table::{ColumnDef, ColumnWidth, DataTable};
use ui::widget::Widget;

/// Column keys in left-to-right display order (AUDIT.md M1). Exposed for
/// parity tests so they can assert the shape without downcasting the built
/// `DataTable<Entry>` widget.
pub const COLUMN_KEYS: &[&str] = &["icon", "name", "size", "modified"];

/// Build the list-view's `ColumnDef<Entry>` set. Split from `build_list` so
/// parity tests can introspect the column shape without constructing the
/// whole widget tree.
pub fn build_columns() -> Vec<ColumnDef<Entry>> {
    vec![
        ColumnDef::new("icon", "", ColumnWidth::Fixed(40.0)),
        ColumnDef::new("name", "Name", ColumnWidth::Flex(1.0)).sortable(true),
        ColumnDef::new("size", "Size", ColumnWidth::Fixed(90.0)).sortable(true),
        ColumnDef::new("modified", "Modified", ColumnWidth::Fixed(150.0)).sortable(true),
    ]
}

/// Construct the `DataTable<Entry>` used by list-mode. The passed `entries`
/// are installed via `set_rows`. `_selected` is reserved for highlight once
/// Task 11 wires the selection model through.
pub fn build_list(entries: &[Entry], _selected: Option<usize>) -> Box<dyn Widget> {
    let mut table: DataTable<Entry> = DataTable::new(build_columns());
    table.set_rows(entries.to_vec());
    Box::new(table)
}
