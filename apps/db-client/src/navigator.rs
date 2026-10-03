//! The sidebar: the saved connections, and under them the tables and views of
//! the one that is open.
//!
//! Both are the toolkit's [`NavList`], so a row looks, selects and moves the
//! way it does in every other app. The connections list is as tall as its rows;
//! the schema list takes the rest of the column and scrolls.

use ui::chrome::{nav_row_height, nav_section_height};
use ui::icons::Icon;
use ui::theme::Theme;
use ui::widget::{NavAction, NavItem, NavList, NavSection, Widget};
use uuid::Uuid;

use crate::frame::{Frame, FrameStyle, Place, Slot, frame_methods};
use crate::link::{ConnectionRow, Intent, Link};
use crate::model::{DatabaseSchema, Engine, RelationKind, RelationRef};

const CONNECTIONS: &str = "connections";
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

fn engine_icon(engine: Engine) -> Icon {
    match engine {
        Engine::Sqlite => Icon::HardDrive,
        Engine::Postgres | Engine::MySql => Icon::Globe,
    }
}

fn connection_section(rows: &[ConnectionRow]) -> NavSection {
    NavSection::new(
        CONNECTIONS,
        "Connections",
        rows.iter()
            .map(|row| {
                let item = NavItem::new(row.id.to_string(), row.name.clone())
                    .icon(engine_icon(row.engine));
                if row.removable {
                    item
                } else {
                    item.without_actions()
                }
            })
            .collect(),
    )
    .trailing_action(NavAction::new("new", Icon::Plus, "New connection"))
}

/// The catalog as sections: the tables and views of the one schema, or one
/// collapsible section per schema when there are several.
fn schema_sections(schema: &DatabaseSchema) -> Vec<NavSection> {
    let single = schema.schemas.len() == 1;
    schema
        .schemas
        .iter()
        .map(|info| {
            let items = info
                .tables
                .iter()
                .map(|table| {
                    let relation = RelationRef {
                        schema: info.name.clone(),
                        name: table.name.clone(),
                    };
                    let item =
                        NavItem::new(relation_key(&relation), table.name.clone()).without_actions();
                    match table.kind {
                        RelationKind::View => item.icon(Icon::Eye),
                        RelationKind::Table => {
                            let item = item.icon(Icon::List);
                            match table.row_count.and_then(|rows| u32::try_from(rows).ok()) {
                                Some(rows) => item.count(rows),
                                None => item,
                            }
                        }
                    }
                })
                .collect();
            let title = if single { "Tables" } else { info.name.as_str() };
            let section = NavSection::new(format!("schema:{}", info.name), title, items);
            if single {
                section
            } else {
                section.collapsible()
            }
        })
        .collect()
}

pub struct Navigator {
    link: Link,
    connections: NavList,
    schema: NavList,
    shown: Vec<ConnectionRow>,
    shown_revision: Option<u64>,
    schema_visible: bool,
    /// Height of the connections list, from the row metrics of the theme the
    /// tree was last built with.
    connections_height: f32,
    frame: Frame,
    structure_dirty: bool,
}

impl Navigator {
    pub fn new(link: &Link) -> Self {
        let ask = link.clone();
        let remove = link.clone();
        let new = link.clone();
        let connections = NavList::new(vec![connection_section(&[])])
            .with_id("db.connections")
            .with_accessible_name("Connections")
            .on_select(move |id| {
                if let Ok(id) = Uuid::parse_str(&id) {
                    ask.ask(Intent::Connect(id));
                }
            })
            .item_actions(
                vec![NavAction::new("remove", Icon::Trash2, "Remove connection")],
                move |id, _action| {
                    if let Ok(id) = Uuid::parse_str(id) {
                        remove.ask(Intent::RemoveConnection(id));
                    }
                },
            )
            .on_section_action(move |_section, _action| new.ask(Intent::NewConnection));
        let open = link.clone();
        let schema = NavList::new(Vec::new())
            .with_id("db.schema")
            .with_accessible_name("Tables")
            .on_select(move |key| {
                if let Some(relation) = parse_relation(&key) {
                    open.ask(Intent::OpenRelation(relation));
                }
            });
        Self {
            link: link.clone(),
            connections,
            schema,
            shown: Vec::new(),
            shown_revision: None,
            schema_visible: false,
            connections_height: 0.0,
            frame: Frame::default(),
            structure_dirty: false,
        }
    }

    fn slots(&mut self) -> Vec<Slot<'_>> {
        let mut slots = vec![Slot::new(
            &mut self.connections,
            Place::Fixed(self.connections_height),
        )];
        if self.schema_visible {
            slots.push(Slot::new(&mut self.schema, Place::Grow));
        }
        slots
    }

    fn children_ref(&self) -> Vec<&dyn Widget> {
        let mut children: Vec<&dyn Widget> = vec![&self.connections];
        if self.schema_visible {
            children.push(&self.schema);
        }
        children
    }

    fn frame_style(&mut self, theme: &Theme) -> FrameStyle {
        let rows = self.shown.len();
        let section = nav_section_height(theme, true);
        let row = nav_row_height(theme);
        let pad = f64::from(theme.tokens.spacing.s2);
        self.connections_height = (section + rows as f64 * row + pad) as f32;
        FrameStyle::default()
    }

    /// Bring the lists up to the published model.
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
        if rows != self.shown {
            self.shown = rows;
            self.connections
                .set_sections(vec![connection_section(&self.shown)]);
            self.structure_dirty = true;
        }
        let active_key = active.map(|id| id.to_string());
        if self.connections.selected_id() != active_key {
            self.connections.set_selected(active_key.as_deref());
        }
        let visible = connected && !schema.schemas.is_empty();
        if visible != self.schema_visible {
            self.schema_visible = visible;
            self.structure_dirty = true;
        }
        if visible && self.shown_revision != Some(revision) {
            self.shown_revision = Some(revision);
            self.schema.set_sections(schema_sections(&schema));
        }
        let selected_key = selected.as_ref().map(relation_key);
        if visible && self.schema.selected_id() != selected_key {
            self.schema.set_selected(selected_key.as_deref());
        }
    }
}

impl Widget for Navigator {
    fn sync(&mut self, state: &mut ui::widget::AppModel, path: &ui::widget::ModelPath) {
        self.refresh();
        for child in self.children_mut() {
            child.sync(state, path);
        }
    }

    frame_methods!();

    fn name(&self) -> &'static str {
        "Navigator"
    }
}
