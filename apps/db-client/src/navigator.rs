//! The sidebar: the saved connections, and under them the tables and views
//! of the open one's database.
//!
//! One toolkit [`NavList`], so a row looks, selects and moves the way it does
//! in every other app, and one selection: the table that is open, or the
//! connection when no table is. A connection says where it connects (its
//! host and port, or its file) on a second line, and the open one carries
//! the active dot. The open database's tables are a section of their own,
//! titled with the connection's name, so the window's filter narrows the
//! tables and never the connections.

use ui::icons::Icon;
use ui::widget::{NavAction, NavItem, NavList, NavSection, RowStatus};
use uuid::Uuid;

use crate::i18n::t;
use crate::link::{ConnectionRow, Intent, Link};
use crate::model::{DatabaseSchema, Engine, RelationKind, RelationRef, SchemaInfo};

const CONNECTIONS: &str = "connections";
const CATALOG: &str = "catalog";
/// Separates a schema from a table in a row id; no catalog name contains it.
const SEPARATOR: char = '\u{1f}';

fn relation_key(relation: &RelationRef) -> String {
    format!("{}{SEPARATOR}{}", relation.schema, relation.name)
}

fn parse_relation(key: &str) -> Option<RelationRef> {
    let (schema, name) = key.split_once(SEPARATOR)?;
    Some(RelationRef {
        schema: schema.to_owned(),
        name: name.to_owned(),
    })
}

fn connection_key(id: Uuid) -> String {
    id.to_string()
}

/// The connection a row id names.
fn parse_connection(key: &str) -> Option<Uuid> {
    Uuid::parse_str(key).ok()
}

fn engine_icon(engine: Engine) -> Icon {
    match engine {
        Engine::Sqlite => Icon::HardDrive,
        Engine::Postgres | Engine::MySql => Icon::Globe,
    }
}

/// The tables and views of one schema.
fn relation_items(schema: &SchemaInfo) -> Vec<NavItem> {
    schema
        .tables
        .iter()
        .map(|table| {
            let relation = RelationRef {
                schema: schema.name.clone(),
                name: table.name.clone(),
            };
            let item = NavItem::new(relation_key(&relation), table.name.clone()).without_actions();
            match table.kind {
                RelationKind::View => item.icon(Icon::Eye),
                RelationKind::Table => {
                    let item = item.icon(Icon::Table);
                    match table.row_count.and_then(|rows| u32::try_from(rows).ok()) {
                        Some(rows) => item.count(rows),
                        None => item,
                    }
                }
            }
        })
        .collect()
}

/// What an open connection holds: the tables of its one schema, or a folder
/// for each when there are several, open whenever the database is opened.
fn catalog_items(schema: &DatabaseSchema) -> Vec<NavItem> {
    if let [only] = schema.schemas.as_slice() {
        return relation_items(only);
    }
    schema
        .schemas
        .iter()
        .map(|info| {
            NavItem::new(format!("schema:{}", info.name), info.name.clone())
                .icon(Icon::Folder)
                .without_actions()
                .children(relation_items(info))
                .ensure_expanded()
        })
        .collect()
}

/// The saved connections; the open one carries the active dot.
fn connection_section(rows: &[ConnectionRow], open: Option<Uuid>) -> NavSection {
    NavSection::new(
        CONNECTIONS,
        t("db-connections"),
        rows.iter()
            .map(|row| {
                let mut item = NavItem::new(connection_key(row.id), row.name.clone())
                    .icon(engine_icon(row.engine));
                if !row.address.is_empty() {
                    item = item.secondary(row.address.clone());
                }
                if open == Some(row.id) {
                    item = item.status(RowStatus::Active);
                }
                if row.removable {
                    item
                } else {
                    item.without_actions()
                }
            })
            .collect(),
    )
    .trailing_action(NavAction::new("new", Icon::Plus, &t("db-new-connection")))
}

/// The open database's tables, under the connection's name; the one
/// section the window's filter narrows.
fn catalog_section(name: &str, schema: &DatabaseSchema) -> NavSection {
    NavSection::new(CATALOG, name, catalog_items(schema))
        .empty_caption(&t("db-tables"))
        .filterable()
}

fn sections(rows: &[ConnectionRow], open: Option<(Uuid, &DatabaseSchema)>) -> Vec<NavSection> {
    let mut sections = vec![connection_section(rows, open.map(|(id, _)| id))];
    if let Some((id, schema)) = open
        && let Some(row) = rows.iter().find(|row| row.id == id)
    {
        sections.push(catalog_section(&row.name, schema));
    }
    sections
}

/// The sidebar list, kept in line with the published model.
pub struct Navigator {
    link: Link,
    list: NavList,
    shown: Vec<ConnectionRow>,
    /// The open connection and the catalog revision the rows were built from.
    shown_open: Option<(Uuid, u64)>,
}

impl Navigator {
    pub fn new(link: &Link) -> Self {
        let ask = link.clone();
        let remove = link.clone();
        let new = link.clone();
        let opened = link.clone();
        let list = NavList::new(sections(&[], None))
            .with_id("db.sidebar")
            .with_accessible_name(&t("db-navigator"))
            .on_select(move |id| {
                if let Some(id) = parse_connection(&id) {
                    // Choosing the connection that is already open keeps it open.
                    let open = opened.read(|model| model.connected && model.active == Some(id));
                    if !open {
                        ask.ask(Intent::Connect(id));
                    }
                } else if let Some(relation) = parse_relation(&id) {
                    ask.ask(Intent::OpenRelation(relation));
                }
            })
            .item_actions(
                vec![NavAction::new(
                    "remove",
                    Icon::Trash2,
                    &t("db-remove-connection"),
                )],
                move |id, _action| {
                    if let Some(id) = parse_connection(id) {
                        remove.ask(Intent::RemoveConnection(id));
                    }
                },
            )
            .on_section_action(move |_section, _action| new.ask(Intent::NewConnection));
        Self {
            link: link.clone(),
            list,
            shown: Vec::new(),
            shown_open: None,
        }
    }

    /// Bring the list up to the published model.
    fn refresh(&mut self) {
        let (rows, active, connected, revision, schema, selected) = self.link.read(|model| {
            (
                model.connections.clone(),
                model.active,
                model.connected,
                model.schema_revision,
                model.schema.clone(),
                model.selected_relation.clone(),
            )
        });
        let open = active.filter(|_| connected);
        let open_now = open.map(|id| (id, revision));
        if rows != self.shown || open_now != self.shown_open {
            self.list
                .set_sections(sections(&rows, open.map(|id| (id, schema.as_ref()))));
            self.shown = rows;
            self.shown_open = open_now;
        }
        // One selection: the open table, or else the connection itself.
        let selected_key = match (&selected, open) {
            (Some(relation), Some(_)) => Some(relation_key(relation)),
            _ => active.map(connection_key),
        };
        if self.list.selected_id() != selected_key {
            self.list.set_selected(selected_key.as_deref());
        }
    }
}

impl ui::widget::Decorator for Navigator {
    type Inner = NavList;

    fn inner(&self) -> &NavList {
        &self.list
    }

    fn inner_mut(&mut self) -> &mut NavList {
        &mut self.list
    }

    fn sync(&mut self, state: &ui::widget::AppModel, path: &ui::widget::ModelPath) {
        self.refresh();
        ui::widget::Widget::sync(&mut self.list, state, path);
    }

    fn name(&self) -> &'static str {
        "Navigator"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::link::ViewModel;
    use crate::model::{ColumnInfo, TableInfo};
    use ui::headless::FrameHarness;
    use ui::theme::Theme;

    fn table(name: &str, kind: RelationKind, rows: Option<i64>) -> TableInfo {
        TableInfo {
            name: name.into(),
            kind,
            columns: vec![ColumnInfo {
                name: "id".into(),
                decl_type: "INTEGER".into(),
                not_null: true,
                primary_key: true,
                default: None,
            }],
            foreign_keys: Vec::new(),
            indexes: Vec::new(),
            row_count: rows,
        }
    }

    fn catalog(schemas: &[(&str, Vec<TableInfo>)]) -> DatabaseSchema {
        DatabaseSchema {
            schemas: schemas
                .iter()
                .map(|(name, tables)| SchemaInfo {
                    name: (*name).into(),
                    tables: tables.clone(),
                })
                .collect(),
        }
    }

    fn row(name: &str, engine: Engine, removable: bool) -> ConnectionRow {
        ConnectionRow {
            id: Uuid::new_v4(),
            name: name.into(),
            engine,
            address: match engine {
                Engine::Sqlite => String::new(),
                Engine::Postgres | Engine::MySql => "db.example.net:5432".into(),
            },
            removable,
        }
    }

    /// The open database's tables are their own filterable section, titled
    /// with the connection, with row counts; several schemas get a folder
    /// each; with no connection open there are no tables.
    #[test]
    fn the_open_connection_holds_its_catalog() {
        let sample = row("Chinook sample", Engine::Sqlite, false);
        let analytics = row("Analytics", Engine::Postgres, true);
        let rows = [sample.clone(), analytics.clone()];

        // --- phase: one_schema_is_just_tables ---
        {
            let schema = catalog(&[(
                "main",
                vec![
                    table("albums", RelationKind::Table, Some(8)),
                    table("recent", RelationKind::View, None),
                ],
            )]);
            let all = sections(&rows, Some((sample.id, &schema)));
            assert_eq!(all.len(), 2, "connections, then the open catalog");
            let connections = &all[0];
            assert!(
                !connections.filterable,
                "the filter never hides connections"
            );
            assert!(
                connections.items[0].status.is_some(),
                "the open one is marked"
            );
            assert!(connections.items[1].status.is_none());
            assert_eq!(
                connections.items[1].secondary.as_deref(),
                Some("db.example.net:5432"),
                "a network connection says where it connects"
            );
            let catalog = &all[1];
            assert!(catalog.filterable);
            assert_eq!(catalog.title, "Chinook sample");
            let tables = &catalog.items;
            let labels: Vec<_> = tables.iter().map(|i| i.label.as_str()).collect();
            assert_eq!(labels, ["albums", "recent"]);
            assert!(tables[0].count.is_some(), "a table shows its rows");
            assert!(tables[1].count.is_none(), "a view has no count");
        }

        // --- phase: several_schemas_are_one_folder_each ---
        {
            let schema = catalog(&[
                (
                    "public",
                    vec![table("orders", RelationKind::Table, Some(3))],
                ),
                (
                    "archive",
                    vec![table("orders", RelationKind::Table, Some(9))],
                ),
            ]);
            let all = sections(&rows, Some((analytics.id, &schema)));
            let folders = &all[1].items;
            assert!(
                folders.iter().all(|folder| folder.ensure_expanded),
                "each schema opens with the database, whatever was folded"
            );
            assert_eq!(
                folders.iter().map(|i| i.label.as_str()).collect::<Vec<_>>(),
                ["public", "archive"]
            );
            assert_ne!(
                folders[0].children[0].id, folders[1].children[0].id,
                "the same table name in two schemas is two rows"
            );
        }

        // --- phase: no_connection_open_means_no_tables ---
        {
            assert_eq!(sections(&rows, None).len(), 1, "only the connections");
        }

        // --- phase: a_row_id_names_its_relation ---
        {
            let relation = RelationRef {
                schema: "other:schema".into(),
                name: "a b".into(),
            };
            assert_eq!(parse_relation(&relation_key(&relation)), Some(relation));
            assert_eq!(parse_relation("no separator"), None);
        }
    }

    /// Choosing a connection, a table, removing a connection and adding one reach the controller as intents; the sample cannot be removed.
    #[test]
    fn the_sidebar_asks_for_connections_to_open_and_to_change() {
        let link = Link::new();
        let sample = row("Chinook sample", Engine::Sqlite, false);
        let analytics = row("Analytics", Engine::Postgres, true);
        link.publish(ViewModel {
            connections: vec![sample.clone(), analytics.clone()],
            active: Some(sample.id),
            connected: true,
            schema: std::rc::Rc::new(catalog(&[(
                "main",
                vec![table("albums", RelationKind::Table, Some(8))],
            )])),
            schema_revision: 1,
            ..ViewModel::default()
        });
        let mut harness = FrameHarness::for_root(
            ui::State::new(),
            Navigator::new(&link),
            (320, 700),
            Theme::dark(),
        );
        harness.relayout();

        // --- phase: the_open_connection_is_selected_until_a_table_is ---
        {
            let navigator = harness.root::<Navigator>();
            assert_eq!(
                navigator.list.selected_id(),
                Some(connection_key(sample.id))
            );
        }

        // --- phase: choosing_a_connection_opens_it ---
        {
            let rect = harness
                .root::<Navigator>()
                .list
                .row_rect(&analytics.id.to_string())
                .expect("the row is on screen");
            harness.click_at(rect.center().x, rect.center().y);
            let intents = link.take_intents();
            assert!(
                intents
                    .iter()
                    .any(|intent| matches!(intent, Intent::Connect(id) if *id == analytics.id)),
                "{intents:?}"
            );
        }

        // --- phase: choosing_the_open_connection_asks_for_nothing ---
        {
            let rect = harness
                .root::<Navigator>()
                .list
                .row_rect(&connection_key(sample.id))
                .expect("the row is on screen");
            harness.click_at(rect.center().x, rect.center().y);
            assert!(
                link.take_intents()
                    .iter()
                    .all(|intent| !matches!(intent, Intent::Connect(_))),
                "it is already open"
            );
        }

        // --- phase: choosing_a_table_opens_it ---
        {
            let key = relation_key(&RelationRef {
                schema: "main".into(),
                name: "albums".into(),
            });
            let rect = harness
                .root::<Navigator>()
                .list
                .row_rect(&key)
                .expect("the table is listed under the connections");
            harness.click_at(rect.center().x, rect.center().y);
            assert!(
                link.take_intents()
                    .iter()
                    .any(|intent| matches!(intent, Intent::OpenRelation(r) if r.name == "albums"))
            );
        }

        // --- phase: the_sample_has_no_remove_action_and_others_do ---
        {
            let section = connection_section(&[sample, analytics], None);
            assert!(
                !section.items[0].actions,
                "the bundled sample cannot be removed"
            );
            assert!(section.items[1].actions);
            assert!(
                section.trailing_action.is_some(),
                "the section adds connections"
            );
        }
    }

    /// Opening a table moves the one selection to it, and closing it moves the selection back to the connection; opening another connection and returning to the first still lists its tables.
    #[test]
    fn one_selection_follows_the_open_table() {
        let link = Link::new();
        let sample = row("Chinook sample", Engine::Sqlite, false);
        let analytics = row("Analytics", Engine::Postgres, true);
        let albums = RelationRef {
            schema: "main".into(),
            name: "albums".into(),
        };
        let schema = std::rc::Rc::new(catalog(&[(
            "main",
            vec![table("albums", RelationKind::Table, Some(8))],
        )]));
        let model = |active: Uuid, relation: Option<RelationRef>, revision: u64| ViewModel {
            connections: vec![sample.clone(), analytics.clone()],
            active: Some(active),
            connected: true,
            schema: std::rc::Rc::clone(&schema),
            schema_revision: revision,
            selected_relation: relation,
            ..ViewModel::default()
        };
        link.publish(model(sample.id, None, 1));
        let mut harness = FrameHarness::for_root(
            ui::State::new(),
            Navigator::new(&link),
            (320, 700),
            Theme::dark(),
        );
        harness.relayout();

        link.publish(model(sample.id, Some(albums.clone()), 1));
        harness.relayout();
        assert_eq!(
            harness.root::<Navigator>().list.selected_id(),
            Some(relation_key(&albums))
        );

        link.publish(model(analytics.id, None, 2));
        harness.relayout();
        assert_eq!(
            harness.root::<Navigator>().list.selected_id(),
            Some(connection_key(analytics.id))
        );

        link.publish(model(sample.id, None, 3));
        harness.relayout();
        assert!(
            harness
                .root::<Navigator>()
                .list
                .row_rect(&relation_key(&albums))
                .is_some(),
            "the connection that was opened again lists its tables"
        );
    }
}
