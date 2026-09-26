//! The schema browser in the sidebar: the toolkit's [`TreeView`] over the
//! connected database — schemas, their tables and views, and each table's
//! columns with a type chip.
//!
//! The tree owns rows, keyboard navigation, disclosure, selection, hover,
//! scrolling and semantics; this module owns the model built from the live
//! [`DatabaseSchema`], each row's decoration, and the activation signal the
//! app turns into "open this table".

use std::cell::RefCell;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::rc::Rc;

use ui::theme::Theme;
use ui::widget::tree_view::{
    NodeId, TreeClickActivation, TreeNode, TreeRowDecoration, TreeRowInfo, TreeRowTone, TreeView,
};

use crate::model::{DatabaseSchema, RelationKind, RelationRef, TypeClass};
use crate::theme_ext::ThemeExt;

/// What one schema-browser row stands for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SchemaRow {
    /// The hidden root.
    Database,
    Schema {
        tables: usize,
    },
    Relation {
        relation: RelationRef,
        view: bool,
        rows: Option<i64>,
    },
    Column {
        relation: RelationRef,
        class: TypeClass,
        /// Declared type plus key/nullability markers, shown after the name.
        detail: String,
    },
}

/// The schema browser widget type the app hosts.
pub type SchemaBrowser = TreeView<SchemaRow>;

/// Queue of row ids the user activated, drained by the app each frame.
pub type SchemaActivations = Rc<RefCell<Vec<NodeId>>>;

fn stable_id(parts: &[&str]) -> NodeId {
    let mut hasher = DefaultHasher::new();
    parts.hash(&mut hasher);
    hasher.finish()
}

fn schema_id(schema: &str) -> NodeId {
    stable_id(&["schema", schema])
}

/// The tree id of a relation's row.
pub fn relation_id(relation: &RelationRef) -> NodeId {
    stable_id(&["relation", &relation.schema, &relation.name])
}

/// Build the browser's model. Ids derive from catalog names, so expansion,
/// selection and scroll position survive a schema refresh.
pub fn model(schema: &DatabaseSchema) -> TreeNode<SchemaRow> {
    let schemas = schema
        .schemas
        .iter()
        .map(|info| {
            let tables = info
                .tables
                .iter()
                .map(|table| {
                    let relation = RelationRef {
                        schema: info.name.clone(),
                        name: table.name.clone(),
                    };
                    let columns = table
                        .columns
                        .iter()
                        .map(|column| {
                            let mut detail = column.decl_type.clone();
                            if column.primary_key {
                                detail = format!("PK  {detail}");
                            } else if column.not_null {
                                detail = format!("NOT NULL  {detail}");
                            }
                            TreeNode::leaf(
                                stable_id(&["column", &info.name, &table.name, &column.name]),
                                column.name.clone(),
                                SchemaRow::Column {
                                    relation: relation.clone(),
                                    class: column.class,
                                    detail,
                                },
                            )
                        })
                        .collect();
                    TreeNode::branch(
                        relation_id(&relation),
                        table.name.clone(),
                        SchemaRow::Relation {
                            view: table.kind == RelationKind::View,
                            rows: table.row_count,
                            relation,
                        },
                        columns,
                    )
                })
                .collect();
            TreeNode::branch(
                schema_id(&info.name),
                info.name.clone(),
                SchemaRow::Schema {
                    tables: info.tables.len(),
                },
                tables,
            )
        })
        .collect();
    TreeNode::branch(stable_id(&["database"]), "", SchemaRow::Database, schemas)
}

/// A structural fingerprint of the catalog: the browser's model is rebuilt
/// only when this changes.
pub fn signature(schema: &DatabaseSchema) -> u64 {
    let mut hasher = DefaultHasher::new();
    for info in &schema.schemas {
        info.name.hash(&mut hasher);
        for table in &info.tables {
            table.name.hash(&mut hasher);
            table.row_count.hash(&mut hasher);
            (table.kind == RelationKind::View).hash(&mut hasher);
            for column in &table.columns {
                column.name.hash(&mut hasher);
                column.decl_type.hash(&mut hasher);
                column.primary_key.hash(&mut hasher);
                column.not_null.hash(&mut hasher);
            }
        }
    }
    hasher.finish()
}

fn decorate(model: &TreeNode<SchemaRow>, row: TreeRowInfo, theme: &Theme) -> TreeRowDecoration {
    let colors = &theme.tokens.colors;
    match model.find(row.id).map(|node| &node.data) {
        Some(SchemaRow::Schema { tables, .. }) => TreeRowDecoration::new()
            .trailing(tables.to_string())
            .description(format!("Schema, {tables} tables")),
        Some(SchemaRow::Relation { view, rows, .. }) => {
            let (glyph, color, kind) = if *view {
                ("◫", colors.semantic.info, "View")
            } else {
                ("▦", theme.accent_primary, "Table")
            };
            let decoration = TreeRowDecoration::new()
                .glyph(glyph, color)
                .description(kind);
            match rows {
                Some(rows) => decoration.trailing(rows.to_string()),
                None => decoration,
            }
        }
        Some(SchemaRow::Column { class, detail, .. }) => TreeRowDecoration::new()
            .badge(class.icon(), theme.type_color(*class))
            .tone(TreeRowTone::Secondary)
            .trailing(detail.clone())
            .description(detail.clone()),
        Some(SchemaRow::Database) | None => TreeRowDecoration::new(),
    }
}

/// What activating a row asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Activation {
    /// Open a relation's data (a column row opens its table).
    Open(RelationRef),
    /// Expand or collapse a schema.
    Toggle(NodeId),
}

/// Resolve an activated row against the browser's model.
pub fn resolve(model: &TreeNode<SchemaRow>, id: NodeId) -> Option<Activation> {
    match &model.find(id)?.data {
        SchemaRow::Relation { relation, .. } | SchemaRow::Column { relation, .. } => {
            Some(Activation::Open(relation.clone()))
        }
        SchemaRow::Schema { .. } => Some(Activation::Toggle(id)),
        SchemaRow::Database => None,
    }
}

/// The tree id of a schema's row.
pub fn schema_row_id(schema: &str) -> NodeId {
    schema_id(schema)
}

/// Build the schema browser over `schema`, pushing activated row ids into
/// `activations`. `main` starts expanded, as the browser always has.
pub fn browser(schema: &DatabaseSchema, activations: &SchemaActivations) -> SchemaBrowser {
    let sink = Rc::clone(activations);
    TreeView::new(model(schema))
        .with_id("db_schema_tree")
        .hide_root()
        .fill()
        .with_heading("SCHEMA")
        .with_accessible_name("Schema")
        .with_ground(|theme| theme.db().sidebar_bg)
        .click_activation(TreeClickActivation::SingleClick)
        .selection_follows_focus(true)
        .with_row_decoration(decorate)
        .with_expanded([schema_id("main")])
        .on_activate(move |id| sink.borrow_mut().push(id))
}

/// Re-read a refreshed catalog into the browser, keeping expansion,
/// selection and scroll anchored to the same rows. The first catalog a
/// connection delivers opens `main`.
pub fn refresh(view: &mut SchemaBrowser, schema: &DatabaseSchema) {
    let first_load = view.visible_len() == 0;
    view.replace_model(model(schema));
    if first_load {
        view.set_expanded(schema_id("main"), true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ColumnInfo, SchemaInfo, TableInfo};

    fn catalog() -> DatabaseSchema {
        let column = |name: &str, decl: &str, primary_key: bool| ColumnInfo {
            name: name.into(),
            decl_type: decl.into(),
            class: TypeClass::classify(decl),
            not_null: primary_key,
            primary_key,
            default: None,
        };
        let table = |name: &str, kind: RelationKind| TableInfo {
            name: name.into(),
            kind,
            columns: vec![
                column("id", "INTEGER", true),
                column("title", "TEXT", false),
            ],
            foreign_keys: Vec::new(),
            indexes: Vec::new(),
            row_count: (kind == RelationKind::Table).then_some(3),
        };
        DatabaseSchema {
            schemas: vec![
                SchemaInfo {
                    name: "main".into(),
                    tables: vec![
                        table("albums", RelationKind::Table),
                        table("recent", RelationKind::View),
                    ],
                },
                SchemaInfo {
                    name: "archive".into(),
                    tables: vec![table("old", RelationKind::Table)],
                },
            ],
        }
    }

    /// Schemas are top level with main open, rows resolve to their activation, the first catalog opens main and expansion survives refresh.
    #[test]
    fn schema_navigation_opens_main_and_survives_refresh() {
        // --- phase: schemas_are_the_top_level_and_main_starts_open ---
        {
            let events = SchemaActivations::default();
            let view = browser(&catalog(), &events);
            let rows: Vec<_> = view
                .visible_rows()
                .into_iter()
                .map(|(_, label, depth)| (label.to_owned(), depth))
                .collect();
            assert_eq!(
                rows,
                [
                    ("main".to_owned(), 0),
                    ("albums".to_owned(), 1),
                    ("recent".to_owned(), 1),
                    ("archive".to_owned(), 0),
                ]
            );
        }

        // --- phase: rows_resolve_to_what_activating_them_means ---
        {
            let model = model(&catalog());
            let albums = RelationRef {
                schema: "main".into(),
                name: "albums".into(),
            };
            assert_eq!(
                resolve(&model, relation_id(&albums)),
                Some(Activation::Open(albums.clone()))
            );
            let column = stable_id(&["column", "main", "albums", "title"]);
            assert_eq!(resolve(&model, column), Some(Activation::Open(albums)));
            assert_eq!(
                resolve(&model, schema_row_id("archive")),
                Some(Activation::Toggle(schema_row_id("archive")))
            );
        }

        // --- phase: the_first_catalog_opens_main ---
        {
            let events = SchemaActivations::default();
            let mut view = browser(&DatabaseSchema::default(), &events);
            assert_eq!(view.visible_len(), 0);
            refresh(&mut view, &catalog());
            assert!(view.is_expanded(schema_row_id("main")));
        }

        // --- phase: expansion_survives_a_catalog_refresh ---
        {
            let events = SchemaActivations::default();
            let mut view = browser(&catalog(), &events);
            let albums = relation_id(&RelationRef {
                schema: "main".into(),
                name: "albums".into(),
            });
            view.set_expanded(albums, true);
            let mut refreshed = catalog();
            if let Some(table) = refreshed.schemas[0].tables.first_mut() {
                table.row_count = Some(4);
            }
            refresh(&mut view, &refreshed);
            assert!(view.is_expanded(albums));
            assert_eq!(view.visible_len(), 6, "albums' two columns stay revealed");
        }
    }
}
