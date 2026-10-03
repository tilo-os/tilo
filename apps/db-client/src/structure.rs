//! The Structure tab: a table's columns, keys and indexes as three tables in
//! one scrolling page. Read-only inspection, the view you reach for to
//! remember a column's type or check a constraint.

use std::rc::Rc;

use ui::TextRole;
use ui::widget::data_table::{ColumnDef, ColumnWidth, DataTable};
use ui::widget::{Column, Decorator, Element, Scrollable, Section, Text, Widget};

use crate::link::Link;
use crate::model::TableInfo;
use crate::results::column_share;

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

/// A column that takes the share of the width its header and cells ask for.
fn column<Row: Clone + 'static>(
    key: &str,
    header: &str,
    rows: &[Row],
    text: impl Fn(&Row) -> String + Send + Sync + 'static,
) -> ColumnDef<Row> {
    let shown: Vec<String> = rows.iter().map(&text).collect();
    let share = column_share(header, shown.iter().map(String::as_str));
    ColumnDef::new(key, header, ColumnWidth::Flex(share)).with_text(text)
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
    let rows: Vec<ColumnRow> = info
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
            column("name", "Name", &rows, |row: &ColumnRow| row.name.clone()),
            column("type", "Type", &rows, |row: &ColumnRow| {
                row.declared.clone()
            })
            .with_kind(ui::text_render::TextKind::Identifier),
            column("nullable", "Nullable", &rows, |row: &ColumnRow| {
                row.nullable.to_owned()
            }),
            column("key", "Key", &rows, |row: &ColumnRow| row.key.to_owned()),
            column("default", "Default", &rows, |row: &ColumnRow| {
                row.default.clone().unwrap_or_else(|| "None".to_owned())
            })
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
                column("constraint", "Constraint", &rows, |row: &KeyRow| {
                    row.constraint.to_owned()
                }),
                column("column", "Column", &rows, |row: &KeyRow| row.column.clone()),
                column("references", "References", &rows, |row: &KeyRow| {
                    row.references.clone().unwrap_or_else(|| "None".to_owned())
                })
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
                column("name", "Index", &rows, |row: &IndexRow| row.name.clone()),
                column("unique", "Unique", &rows, |row: &IndexRow| {
                    row.unique.to_owned()
                }),
                column("columns", "Columns", &rows, |row: &IndexRow| {
                    row.columns.clone()
                }),
            ],
            rows,
        )
    })
}

fn page(info: &TableInfo) -> Element {
    let mut column = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .push(Box::new(
            Text::new(&info.name)
                .with_role(TextRole::Title)
                .with_kind(ui::text_render::TextKind::Identifier),
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
    link: Link,
    content: Element,
    shown: Option<Rc<TableInfo>>,
    structure_dirty: bool,
}

impl StructureView {
    pub fn new(link: &Link) -> Self {
        Self {
            link: link.clone(),
            content: Box::new(Text::new("")),
            shown: None,
            structure_dirty: false,
        }
    }

    /// Show `table`, or nothing when it is `None`.
    fn show(&mut self, table: Option<&Rc<TableInfo>>) {
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

impl Decorator for StructureView {
    type Inner = dyn Widget;

    fn inner(&self) -> &(dyn Widget + 'static) {
        self.content.as_ref()
    }

    fn inner_mut(&mut self) -> &mut (dyn Widget + 'static) {
        self.content.as_mut()
    }

    fn sync(&mut self, state: &mut ui::widget::AppModel, path: &ui::widget::ModelPath) {
        let table = self.link.read(|model| model.table.clone());
        self.show(table.as_ref());
        self.content.sync(state, path);
    }

    fn take_structure_dirty(&mut self) -> bool {
        let rebuilt = std::mem::take(&mut self.structure_dirty);
        self.content.take_structure_dirty() || rebuilt
    }

    fn name(&self) -> &'static str {
        "StructureView"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ColumnInfo, ForeignKey, IndexInfo, RelationKind, TypeClass};

    fn info(rows: Option<i64>) -> TableInfo {
        let column = |name: &str, primary_key: bool| ColumnInfo {
            name: name.into(),
            decl_type: "INTEGER".into(),
            class: TypeClass::Integer,
            not_null: false,
            primary_key,
            default: None,
        };
        TableInfo {
            name: "tracks".into(),
            kind: RelationKind::Table,
            columns: vec![column("id", true), column("album_id", false)],
            foreign_keys: vec![ForeignKey {
                column: "album_id".into(),
                ref_table: "albums".into(),
                ref_column: "id".into(),
            }],
            indexes: vec![IndexInfo {
                name: "idx_tracks_album".into(),
                unique: false,
                columns: vec!["album_id".into()],
            }],
            row_count: rows,
        }
    }

    /// A primary key is not nullable, and the keys table lists the primary key before the foreign keys.
    #[test]
    fn keys_and_nullability_follow_the_catalog() {
        let table = info(Some(3));
        let keys = keys_table(&table).expect("the table has keys");
        let rows = keys.view_rows();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].constraint, "Primary key");
        assert_eq!(rows[1].constraint, "Foreign key");
        assert_eq!(rows[1].references.as_deref(), Some("albums.id"));
        let columns = columns_table(&table);
        let rows = columns.view_rows();
        assert_eq!(rows[0].nullable, "No", "the primary key cannot be null");
        assert_eq!(rows[1].nullable, "Yes");
        assert!(indexes_table(&table).is_some());
    }
}
