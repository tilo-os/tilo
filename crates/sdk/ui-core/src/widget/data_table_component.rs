//! [`DataTable`] as a described component: columns and rows as parts, each row holding its cells' text
//! by the column it belongs to, and selecting or activating a row reported with the resource it stands
//! for.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::component::{Component, Described, Event, ResourceId, Value};
use crate::selection::SelectionModel;
use crate::text_render::TextKind;
use crate::widget::Widget;
use crate::widget::data_table::{ColumnDef, ColumnWidth, DataTable};

crate::component::choice!(crate::text_render::TextKind, "TextKind", {
    Prose => "prose",
    Numeric => "numeric",
    Secret => "secret",
    Identifier => "identifier",
    Url => "url",
    Code => "code",
});

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TableColumnWidth {
    #[default]
    Auto,
    Flex,
    Fixed,
}

crate::component::choice!(TableColumnWidth, "TableColumnWidth", {
    Auto => "auto",
    Flex => "flex",
    Fixed => "fixed",
});

/// One column of a table: what it is called, how wide it is, and how its cells read.
#[derive(Clone, Component, Debug, Default, PartialEq)]
#[component(name = "DataTableColumn", role = "Cell", label = "header")]
pub struct DataTableColumnProps {
    /// What the column is called, as its header, and the key its cells are held under in a row.
    #[component(required, positional)]
    pub header: String,
    /// How wide it is: as wide as its content, one share of the spare room, or `pixels` wide.
    pub width: Option<TableColumnWidth>,
    /// Its width in pixels, when `width` is `fixed`.
    pub pixels: Option<f64>,
    /// How its cells read: prose unless said.
    pub kind: Option<TextKind>,
    /// How many lines a cell's text wraps to before it is elided; the row grows to hold them.
    pub lines: Option<i64>,
    /// Whether pressing its header sorts the table by it.
    pub sortable: bool,
    /// Which columns give way first when the table is narrow: `0` never hides, higher numbers hide before
    /// lower ones.
    pub priority: i64,
    /// What an empty cell reads as, dimmed.
    pub absent: Option<String>,
}

/// One cell of a row: what its column reads in this row.
#[derive(Clone, Component, Default, PartialEq)]
#[component(name = "DataTableCell", role = "Cell")]
pub struct DataTableCellProps {
    /// The header of the column it stands under.
    #[component(required, positional)]
    pub column: String,
    /// What it reads: absent (dimmed) when not given.
    pub value: Option<String>,
}

/// One row of a table: the resource it stands for, and the cells its columns read in it.
#[derive(Clone, Component, Default)]
#[component(name = "DataTableRow", role = "Row", presents = "item")]
pub struct DataTableRowProps {
    /// The resource the row stands for: its stable key, and what selecting or activating it reports.
    #[component(required, positional)]
    pub item: ResourceId,
    /// The cells, in the order of the table's columns.
    #[component(slot(default))]
    pub cells: Vec<DataTableCellProps>,
}

impl DataTableRowProps {
    #[must_use]
    pub fn cell(&self, header: &str) -> Option<&str> {
        self.cells
            .iter()
            .find(|cell| cell.column == header)
            .and_then(|cell| cell.value.as_deref())
    }
}

impl PartialEq for DataTableRowProps {
    fn eq(&self, other: &Self) -> bool {
        self.item == other.item && self.cells == other.cells
    }
}

crate::component::part_slot!(DataTableColumnProps);
crate::component::part_slot!(DataTableRowProps);
crate::component::part_slot!(DataTableCellProps);

/// A table of resources as columns and rows: the attributes of a device, the songs in a library, the
/// rules of a firewall.
#[derive(Component, Default)]
#[component(name = "DataTable", role = "Table", label = "name")]
pub struct DataTableProps {
    /// What the table is, for assistive technology.
    pub name: Option<String>,
    /// The table takes the height its parent gives it and scrolls inside itself, rather than being as tall
    /// as its rows inside another scroll.
    pub fill: bool,
    /// Rows a finger's target tall, rather than a pointer target and a step.
    pub roomy: bool,
    /// Whether pressing a row picks it.
    pub selectable: bool,
    /// The columns, in order.
    #[component(slot)]
    pub columns: Vec<DataTableColumnProps>,
    /// The rows, in order.
    #[component(slot(default))]
    pub rows: Vec<DataTableRowProps>,
    /// The selected rows, by the resource each stands for.
    #[component(state)]
    pub selection: Option<Vec<ResourceId>>,
    /// A row was selected: carries the resource it shows.
    #[component(event(value = ResourceId))]
    pub select: Event,
    /// A row was activated (a double click, Enter, an assistive technology): carries the resource it shows.
    #[component(event(value = ResourceId))]
    pub activate: Event,
}

/// The rows a table shows now, as its events read them: the closures a built table keeps find the
/// row a press landed on by its index among the rows the view gives, so a changed list keeps
/// reporting the right resource.
type SharedRows = Rc<RefCell<Rc<Vec<DataTableRowProps>>>>;

#[derive(Clone, Debug, PartialEq)]
struct ColumnFace {
    header: String,
    width: Option<TableColumnWidth>,
    pixels: Option<f64>,
    kind: Option<TextKind>,
    lines: Option<i64>,
    sortable: bool,
    priority: i64,
    absent: Option<String>,
}

impl ColumnFace {
    fn of(column: &DataTableColumnProps) -> Self {
        Self {
            header: column.header.clone(),
            width: column.width,
            pixels: column.pixels,
            kind: column.kind,
            lines: column.lines,
            sortable: column.sortable,
            priority: column.priority,
            absent: column.absent.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
struct Face {
    name: Option<String>,
    fill: bool,
    roomy: bool,
    selectable: bool,
    columns: Vec<ColumnFace>,
}

pub struct DescribedTable {
    table: DataTable<DataTableRowProps>,
    rows: SharedRows,
    face: Face,
}

impl DescribedTable {
    #[must_use]
    pub fn table(&self) -> &DataTable<DataTableRowProps> {
        &self.table
    }

    pub fn table_mut(&mut self) -> &mut DataTable<DataTableRowProps> {
        &mut self.table
    }
}

fn width_of(column: &DataTableColumnProps) -> ColumnWidth {
    match column.width.unwrap_or_default() {
        TableColumnWidth::Auto => ColumnWidth::Auto,
        TableColumnWidth::Flex => ColumnWidth::Flex(1.0),
        TableColumnWidth::Fixed => ColumnWidth::Fixed(crate::num::f32_of_f64(
            column.pixels.unwrap_or(120.0).max(1.0),
        )),
    }
}

/// The table's columns as the core reads them: each column's cell text read from a row by the
/// column's header.
fn columns_of(columns: &[DataTableColumnProps]) -> Vec<ColumnDef<DataTableRowProps>> {
    columns
        .iter()
        .map(|column| {
            let key = column.header.clone();
            let absent = column.absent.clone().unwrap_or_else(|| "—".to_owned());
            let mut def = ColumnDef::new(key.clone(), column.header.clone(), width_of(column))
                .with_optional_text(
                    move |row: &DataTableRowProps| row.cell(&key).map(str::to_owned),
                    absent,
                );
            if let Some(kind) = column.kind {
                def = def.with_kind(kind);
            }
            if let Some(lines) = column.lines {
                def = def.wrapping(usize::try_from(lines).unwrap_or(1));
            }
            if column.sortable {
                def = def.sortable(true);
            }
            def.priority = u8::try_from(column.priority.clamp(0, i64::from(u8::MAX))).unwrap_or(0);
            def
        })
        .collect()
}

/// Where the selected resources stand among `rows`, in display order, and the first of them.
fn selection_indices(
    keys: &[ResourceId],
    rows: &[DataTableRowProps],
) -> (Vec<usize>, Option<usize>) {
    let at: HashMap<&str, usize> = rows
        .iter()
        .enumerate()
        .map(|(index, row)| (row.item.0.as_str(), index))
        .collect();
    let mut indices: Vec<usize> = keys
        .iter()
        .filter_map(|key| at.get(key.0.as_str()).copied())
        .collect();
    indices.sort_unstable();
    indices.dedup();
    let primary = indices.first().copied();
    (indices, primary)
}

fn row_key(row: &DataTableRowProps) -> u64 {
    use std::hash::{Hash as _, Hasher as _};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    row.item.0.hash(&mut hasher);
    hasher.finish()
}

fn item_at(rows: &SharedRows, index: usize) -> Option<Value> {
    let rows = Rc::clone(&*rows.borrow());
    rows.get(index)
        .map(|row| Value::Resource(row.item.0.clone()))
}

impl Described for DescribedTable {
    type Props = DataTableProps;

    fn from_props(key: &str, props: DataTableProps) -> Self {
        let DataTableProps {
            name,
            fill,
            roomy,
            selectable,
            columns,
            rows,
            selection,
            select,
            activate,
        } = props;
        let rows = Rc::new(rows);
        let shared: SharedRows = Rc::new(RefCell::new(Rc::clone(&rows)));
        let mut table = DataTable::new(columns_of(&columns))
            .with_id(key.to_owned())
            .with_row_key(row_key);
        if let Some(name) = &name {
            table = table.with_accessible_name(name.clone());
        }
        if fill {
            table = table.fill_height();
        }
        if !roomy {
            table = table.compact();
        }
        if selectable || !select.is_none() {
            table = table.selectable(true);
        }
        if !select.is_none() {
            let reading = Rc::clone(&shared);
            table = table.on_row_click_view(format!("{key}.select"), move |index, _, _| {
                if let Some(item) = item_at(&reading, index) {
                    select.fire(Some(item));
                }
            });
        }
        if !activate.is_none() {
            let reading = Rc::clone(&shared);
            table = table.on_activate_view(format!("{key}.activate"), move |index| {
                if let Some(item) = item_at(&reading, index) {
                    activate.fire(Some(item));
                }
            });
        }
        if let Some(keys) = &selection {
            let (indices, primary) = selection_indices(keys, &rows);
            let mut chosen = SelectionModel::new();
            chosen.replace_indices(indices, primary);
            table = table.with_selection(chosen);
        }
        table.set_rows((*rows).clone());
        Self {
            table,
            rows: shared,
            face: Face {
                name,
                fill,
                roomy,
                selectable,
                columns: columns.iter().map(ColumnFace::of).collect(),
            },
        }
    }

    /// Changed rows, a changed selection and a changed name are taken in place, so the scroll, the row
    /// under the pointer and the column widths a person set stay; the sort the table keeps re-applies
    /// to the new rows.
    fn set_props(&mut self, props: DataTableProps) -> bool {
        let DataTableProps {
            name,
            fill,
            roomy,
            selectable,
            columns,
            rows,
            selection,
            select: _,
            activate: _,
        } = props;
        let face = Face {
            name,
            fill,
            roomy,
            selectable,
            columns: columns.iter().map(ColumnFace::of).collect(),
        };
        if face.columns != self.face.columns
            || face.fill != self.face.fill
            || face.roomy != self.face.roomy
        {
            return false;
        }
        if face.name != self.face.name {
            return false;
        }
        let rows = Rc::new(rows);
        let changed = **self.rows.borrow() != *rows;
        *self.rows.borrow_mut() = Rc::clone(&rows);
        if changed {
            self.table.set_rows((*rows).clone());
        }
        if let Some(keys) = &selection {
            let current: Vec<String> = self
                .table
                .selected_rows()
                .into_iter()
                .filter_map(|index| rows.get(index).map(|row| row.item.0.clone()))
                .collect();
            let wanted: Vec<String> = keys.iter().map(|key| key.0.clone()).collect();
            if current != wanted {
                let (indices, primary) = selection_indices(keys, &rows);
                let mut chosen = SelectionModel::new();
                chosen.replace_indices(indices, primary);
                self.table.set_selection(chosen);
            }
        }
        true
    }
}

impl crate::widget::Decorator for DescribedTable {
    type Inner = dyn Widget;

    fn inner(&self) -> &(dyn Widget + 'static) {
        &self.table
    }

    fn inner_mut(&mut self) -> &mut (dyn Widget + 'static) {
        &mut self.table
    }

    fn name(&self) -> &'static str {
        "DataTable"
    }

    fn take_structure_change(&mut self) -> crate::widget::StructureChange {
        self.table.take_structure_change()
    }
}
