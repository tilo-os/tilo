//! The Structure tab: a table's columns, keys and indexes as three tables in
//! one scrolling page. Read-only inspection, the view you reach for to
//! remember a column's type or check a constraint.

use std::rc::Rc;

use ui::TextRole;
use ui::widget::data_table::{ColumnDef, ColumnWidth, DataTable};
use ui::widget::{Column, Decorator, Element, Scrollable, Section, Text, Widget};

use crate::model::{RelationKind, TableInfo};

#[derive(Clone)]
struct ColumnRow {
    name: String,
    declared: String,
    nullable: &'static str,
    key: &'static str,
    default: Option<String>,
}

#[derive(Clone)]
struct KeyRow {
    constraint: &'static str,
    column: String,
    references: Option<String>,
}

#[derive(Clone)]
struct IndexRow {
    name: String,
    unique: &'static str,
    columns: String,
}

fn table<Row: Clone + 'static>(
    name: &str,
    columns: Vec<ColumnDef<Row>>,
    rows: Vec<Row>,
) -> DataTable<Row> {
    let mut table = DataTable::new(columns)
        .with_id(name)
        .with_accessible_name(name);
    table.set_rows(rows);
    table
}

fn columns_table(info: &TableInfo) -> DataTable<ColumnRow> {
    let rows = info
        .columns
        .iter()
        .map(|column| ColumnRow {
            name: column.name.clone(),
            declared: column.decl_type.clone(),
            nullable: if column.not_null || column.primary_key {
                "No"
            } else {
                "Yes"
            },
            key: if column.primary_key { "Primary" } else { "" },
            default: column.default.clone(),
        })
        .collect();
    table(
        "db.structure.columns",
        vec![
            ColumnDef::new("name", "Name", ColumnWidth::Flex(2.0))
                .with_text(|row: &ColumnRow| row.name.clone()),
            ColumnDef::new("type", "Type", ColumnWidth::Flex(2.0))
                .with_kind(ui::text_render::TextKind::Identifier)
                .with_text(|row: &ColumnRow| row.declared.clone()),
            ColumnDef::new("nullable", "Nullable", ColumnWidth::Flex(1.0))
                .with_text(|row: &ColumnRow| row.nullable.to_owned()),
            ColumnDef::new("key", "Key", ColumnWidth::Flex(1.0))
                .with_text(|row: &ColumnRow| row.key.to_owned()),
            ColumnDef::new("default", "Default", ColumnWidth::Flex(2.0))
                .with_kind(ui::text_render::TextKind::Code)
                .with_optional_text(|row: &ColumnRow| row.default.clone(), "None"),
        ],
        rows,
    )
}

fn keys_table(info: &TableInfo) -> Option<DataTable<KeyRow>> {
    let mut rows: Vec<KeyRow> = info
        .primary_keys()
        .into_iter()
        .map(|column| KeyRow {
            constraint: "Primary key",
            column: column.name.clone(),
            references: None,
        })
        .collect();
    rows.extend(info.foreign_keys.iter().map(|key| KeyRow {
        constraint: "Foreign key",
        column: key.column.clone(),
        references: Some(format!("{}.{}", key.ref_table, key.ref_column)),
    }));
    (!rows.is_empty()).then(|| {
        table(
            "db.structure.keys",
            vec![
                ColumnDef::new("constraint", "Constraint", ColumnWidth::Flex(2.0))
                    .with_text(|row: &KeyRow| row.constraint.to_owned()),
                ColumnDef::new("column", "Column", ColumnWidth::Flex(2.0))
                    .with_text(|row: &KeyRow| row.column.clone()),
                ColumnDef::new("references", "References", ColumnWidth::Flex(3.0))
                    .with_kind(ui::text_render::TextKind::Identifier)
                    .with_optional_text(|row: &KeyRow| row.references.clone(), "None"),
            ],
            rows,
        )
    })
}

fn indexes_table(info: &TableInfo) -> Option<DataTable<IndexRow>> {
    let rows: Vec<IndexRow> = info
        .indexes
        .iter()
        .map(|index| IndexRow {
            name: index.name.clone(),
            unique: if index.unique { "Yes" } else { "No" },
            columns: index.columns.join(", "),
        })
        .collect();
    (!rows.is_empty()).then(|| {
        table(
            "db.structure.indexes",
            vec![
                ColumnDef::new("name", "Index", ColumnWidth::Flex(3.0))
                    .with_text(|row: &IndexRow| row.name.clone()),
                ColumnDef::new("unique", "Unique", ColumnWidth::Flex(1.0))
                    .with_text(|row: &IndexRow| row.unique.to_owned()),
                ColumnDef::new("columns", "Columns", ColumnWidth::Flex(3.0))
                    .with_text(|row: &IndexRow| row.columns.clone()),
            ],
            rows,
        )
    })
}

/// "Table with 6 columns and 347 rows" / "View with 6 columns".
fn summary(info: &TableInfo) -> String {
    let kind = match info.kind {
        RelationKind::Table => "Table",
        RelationKind::View => "View",
    };
    let columns = info.columns.len();
    let columns = if columns == 1 {
        "1 column".to_owned()
    } else {
        format!("{columns} columns")
    };
    match info.row_count {
        Some(1) => format!("{kind} with {columns} and 1 row"),
        Some(rows) => format!("{kind} with {columns} and {rows} rows"),
        None => format!("{kind} with {columns}"),
    }
}

fn page(info: &TableInfo) -> Element {
    let mut column = Column::new()
        .with_intrinsic_height()
        .push(Box::new(
            Text::new(&info.name)
                .with_role(TextRole::Title)
                .with_kind(ui::text_render::TextKind::Identifier),
        ))
        .push(Box::new(
            Text::new(&summary(info))
                .with_role(TextRole::Caption)
                .dimmed(),
        ))
        .push(Box::new(Section::titled(
            "Columns",
            vec![Box::new(columns_table(info))],
        )));
    if let Some(keys) = keys_table(info) {
        column = column.push(Box::new(Section::titled("Keys", vec![Box::new(keys)])));
    }
    if let Some(indexes) = indexes_table(info) {
        column = column.push(Box::new(Section::titled(
            "Indexes",
            vec![Box::new(indexes)],
        )));
    }
    Box::new(
        Scrollable::new_vertical()
            .with_child(Box::new(column))
            .fill_parent_height(),
    )
}

/// The page for the selected table. It is rebuilt when the table changes.
pub struct StructureView {
    content: Element,
    shown: Option<Rc<TableInfo>>,
    structure_dirty: bool,
}

impl StructureView {
    pub fn new() -> Self {
        Self {
            content: Box::new(Text::new("")),
            shown: None,
            structure_dirty: false,
        }
    }

    /// Show `table`, or nothing when it is `None`.
    pub fn show(&mut self, table: Option<&Rc<TableInfo>>) {
        let same = match (&self.shown, table) {
            (Some(shown), Some(next)) => Rc::ptr_eq(shown, next),
            (None, None) => true,
            _ => false,
        };
        if same {
            return;
        }
        self.shown = table.cloned();
        self.content = match table {
            Some(info) => page(info),
            None => Box::new(Text::new("")),
        };
        self.structure_dirty = true;
    }
}

impl Default for StructureView {
    fn default() -> Self {
        Self::new()
    }
}

impl Decorator for StructureView {
    type Inner = dyn Widget;

    fn inner(&self) -> &(dyn Widget + 'static) {
        self.content.as_ref()
    }

    fn inner_mut(&mut self) -> &mut (dyn Widget + 'static) {
        self.content.as_mut()
    }

    fn take_structure_dirty(&mut self) -> bool {
        let rebuilt = std::mem::take(&mut self.structure_dirty);
        self.content.take_structure_dirty() || rebuilt
    }

    fn name(&self) -> &'static str {
        "StructureView"
    }
}
