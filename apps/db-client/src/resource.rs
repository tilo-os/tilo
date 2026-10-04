//! Database client as a [`tilo_resource::Resource`]: connection profiles and
//! saved queries sync (passwords stay on the device); connect / run SQL are
//! Effects.

use std::rc::Rc;

use ui::framework::{
    Dispatcher, EffectAnswer, EffectCx, EffectStatus, Fixture, LiveRow, ResourceApp,
    ResourceSurface, SurfaceCx, ViewCx,
};
use ui::tilo_actions::{
    ActionAddress, ActionArgs, ActionParam, ParamType, Resource as PublishedResource,
};
use ui::tilo_resource::items::{ItemAction, ItemCollection, item_children, item_resource_id};
use ui::tilo_resource::{
    self, ActionDecl, Doc, Effect, Id, Invoke, ModelError, PlainText, Resource, SearchEntry,
    TextEdit, child_resource_id, publish_child,
};
use ui::widget::Element;
use uuid::Uuid;

use crate::actions::{DB_ACTION_TYPE, DbAction, register};
use crate::app::DbClientApp;
use crate::doc::{ConnectionsDoc, EditorDoc, SavedQueriesDoc, SavedQuery};
use crate::model::{ConnectionProfile, Engine};

/// Shared catalogue projection (no passwords, no live session).
#[derive(Clone, Debug, Default)]
pub struct DbState {
    pub connections: Vec<ConnectionProfile>,
    pub saved_queries: Vec<SavedQuery>,
    /// Collaborative SQL buffer — binds "Save current query".
    pub editor_sql: String,
}

/// The database workspace resource.
pub struct DbClient;

/// The collection field saved connections are published under.
const CONNECTIONS: &str = "connections";
/// The collection field saved queries are published under.
const SAVED_QUERIES: &str = "saved_queries";

/// The item actions of saved connections and saved queries, offered on each
/// item with its id bound.
const ITEM_ACTIONS: &[ItemAction] = &[
    ItemAction {
        action: "rename_connection",
        param: "connection",
        collection: Some(CONNECTIONS),
    },
    ItemAction {
        action: "remove_connection",
        param: "connection",
        collection: Some(CONNECTIONS),
    },
    ItemAction {
        action: "remove_query",
        param: "query",
        collection: Some(SAVED_QUERIES),
    },
];

fn text_arg(args: &ActionArgs, name: &str) -> Result<String, String> {
    args.text(name)
        .map(str::to_owned)
        .ok_or_else(|| format!("`{name}` is required"))
}

fn item_arg(args: &ActionArgs, name: &str) -> Result<Uuid, String> {
    Uuid::parse_str(&text_arg(args, name)?).map_err(|_| format!("`{name}` is not an item id"))
}

impl Resource for DbClient {
    const KIND: &'static str = "database.workspace";
    const LABEL: &'static str = "Database";
    const ACTION_TYPE: &'static str = DB_ACTION_TYPE;

    type State = DbState;
    type Action = DbAction;

    fn register(registry: &mut tilo_actions::ActionRegistry) {
        register(registry);
    }

    fn read(doc: &Doc) -> Result<DbState, ModelError> {
        Ok(DbState {
            connections: ConnectionsDoc::read(doc),
            saved_queries: SavedQueriesDoc::read(doc),
            editor_sql: EditorDoc::read_sql(doc),
        })
    }

    /// A new workspace starts with the containers every replica writes into,
    /// the SQL buffer among them, so two people who open it before syncing
    /// type into the same text.
    fn initialize(doc: &mut Doc) -> Result<(), ModelError> {
        crate::doc::create_schema(doc).map_err(|error| ModelError::invalid(error.to_string()))
    }

    fn declarations() -> Vec<ActionDecl<Self>> {
        vec![
            ActionDecl::new(
                "add_connection",
                "Add connection",
                vec![
                    ActionParam::required("name", ParamType::Text, "Connection name"),
                    ActionParam::required("engine", ParamType::Text, "Database engine")
                        .with_choices(["sqlite", "postgres", "mysql"]),
                    ActionParam::optional("host", ParamType::Text, "Server host"),
                    ActionParam::optional("port", ParamType::Integer, "Server port"),
                    ActionParam::optional("database", ParamType::Text, "Database name"),
                    ActionParam::optional("user", ParamType::Text, "User name"),
                    ActionParam::optional("file_path", ParamType::Text, "SQLite file"),
                ],
                |args, _| {
                    let engine = Engine::from_key(&text_arg(args, "engine")?);
                    let mut profile = ConnectionProfile::new(&text_arg(args, "name")?, engine);
                    if let Some(host) = args.text("host") {
                        host.clone_into(&mut profile.host);
                    }
                    if let Some(port) = args.integer("port") {
                        profile.port = u16::try_from(port).map_err(|_| "`port` is out of range")?;
                    }
                    for (name, field) in [
                        ("database", &mut profile.database),
                        ("user", &mut profile.user),
                        ("file_path", &mut profile.file_path),
                    ] {
                        if let Some(value) = args.text(name) {
                            value.clone_into(field);
                        }
                    }
                    Ok(crate::actions::add_connection_action(&profile))
                },
            )
            .describe("Save a connection profile; its password stays on each device"),
            ActionDecl::new(
                "rename_connection",
                "Rename connection",
                vec![
                    ActionParam::required("connection", ParamType::Text, "Connection id"),
                    ActionParam::required("name", ParamType::Text, "Connection name"),
                ],
                |args, _| {
                    Ok(crate::actions::rename_connection_action(
                        item_arg(args, "connection")?,
                        &text_arg(args, "name")?,
                    ))
                },
            ),
            ActionDecl::new(
                "remove_connection",
                "Remove saved connection",
                vec![ActionParam::required(
                    "connection",
                    ParamType::Text,
                    "Connection id",
                )],
                |args, _| {
                    Ok(crate::actions::remove_connection_action(item_arg(
                        args,
                        "connection",
                    )?))
                },
            )
            .destructive(),
            ActionDecl::new(
                "save_query",
                "Save query",
                vec![
                    ActionParam::required("name", ParamType::Text, "Query name"),
                    ActionParam::required("sql", ParamType::Text, "SQL"),
                ],
                |args, _| {
                    Ok(crate::actions::save_query_action(
                        Uuid::new_v4(),
                        &text_arg(args, "name")?,
                        &text_arg(args, "sql")?,
                    ))
                },
            ),
            ActionDecl::new(
                "remove_query",
                "Remove saved query",
                vec![ActionParam::required("query", ParamType::Text, "Query id")],
                |args, _| {
                    Ok(crate::actions::remove_query_action(item_arg(
                        args, "query",
                    )?))
                },
            )
            .destructive(),
        ]
    }

    /// "Save current query" names the SQL in the shared buffer.
    fn bound_declarations(state: &DbState) -> Vec<ActionDecl<Self>> {
        let sql = state.editor_sql.clone();
        if sql.trim().is_empty() {
            return Vec::new();
        }
        vec![ActionDecl::new(
            "save_current_query",
            "Save current query",
            vec![ActionParam::required(
                tilo_actions::PROMPTED_PARAM,
                ParamType::Text,
                "Query name",
            )],
            move |args, _| {
                Ok(crate::actions::save_query_action(
                    Uuid::new_v4(),
                    &text_arg(args, tilo_actions::PROMPTED_PARAM)?,
                    &sql,
                ))
            },
        )]
    }

    fn children(state: &DbState) -> Vec<PublishedResource> {
        let collections = [
            ItemCollection {
                field: CONNECTIONS,
                items: state
                    .connections
                    .iter()
                    .map(|profile| (Id::from_raw(profile.id.to_string()), profile.name.clone()))
                    .collect(),
            },
            ItemCollection {
                field: SAVED_QUERIES,
                items: state
                    .saved_queries
                    .iter()
                    .map(|query| (Id::from_raw(query.id.to_string()), query.name.clone()))
                    .collect(),
            },
        ];
        let mut children = item_children::<Self>(&collections, ITEM_ACTIONS);
        for child in &mut children {
            if let Some(profile) = state
                .connections
                .iter()
                .find(|profile| connection_resource_id(profile.id) == child.id.as_str())
            {
                child
                    .actions
                    .push(crate::actions::connect_operation(profile.id));
            } else if let Some(query) = state
                .saved_queries
                .iter()
                .find(|query| saved_query_resource_id(query.id) == child.id.as_str())
            {
                child
                    .actions
                    .push(crate::actions::run_saved_query_operation(query));
            }
        }
        children.push(publish_child::<PlainText, Self>(
            &sql_resource_id(),
            "SQL",
            Some(&state.editor_sql),
            DbAction::EditSql,
        ));
        children
    }

    fn search(state: &DbState) -> Vec<SearchEntry> {
        // Saved SQL can contain credentials or tokens: a saved query is
        // found by its intentional name, never by arbitrary SQL literals.
        let connections = state.connections.iter().map(|profile| {
            SearchEntry::new(
                &profile.name,
                format!("{} {}", profile.engine.as_key(), profile.host),
            )
            .anchored(connection_resource_id(profile.id))
            .invoking(ActionAddress::new(
                connection_resource_id(profile.id),
                "connect",
            ))
        });
        let queries = state.saved_queries.iter().map(|query| {
            SearchEntry::new(&query.name, "Collaborative saved query")
                .anchored(saved_query_resource_id(query.id))
                .invoking(ActionAddress::new(saved_query_resource_id(query.id), "run"))
        });
        connections.chain(queries).collect()
    }
}

/// The id a saved connection is published under.
pub(crate) fn connection_resource_id(id: Uuid) -> String {
    item_resource_id::<DbClient>(CONNECTIONS, &Id::from_raw(id.to_string()))
}

/// The id a saved query is published under.
pub(crate) fn saved_query_resource_id(id: Uuid) -> String {
    item_resource_id::<DbClient>(SAVED_QUERIES, &Id::from_raw(id.to_string()))
}

/// The id the SQL buffer is published under, as a plain-text child of the
/// workspace.
pub(crate) fn sql_resource_id() -> String {
    child_resource_id::<DbClient>("sql")
}

/// Edits of the SQL buffer through the workspace's dispatcher: each one a
/// [`DbAction::EditSql`], addressed to the child [`DbClient::children`]
/// publishes.
pub(crate) fn sql_dispatcher(dispatcher: &Dispatcher<DbClient>) -> Dispatcher<PlainText> {
    dispatcher.child::<PlainText>(&sql_resource_id(), DbAction::EditSql)
}

/// The client's work on a database server, which changes no document: run
/// SQL on the open connection. Connecting, browsing a table and running a
/// saved query ride the rows they act on.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "db_client.effects")]
pub enum DbEffect {
    /// Run SQL
    ///
    /// Runs a statement on the open connection; it may change the database.
    #[effect(job, cancellable, destructive)]
    Query {
        #[param("SQL query")]
        sql: String,
    },
}

/// What a capture shows in place of a database server: the bundled sample
/// catalog, opened for the connection with this id.
pub(crate) struct SampleDatabase {
    pub(crate) connection: Uuid,
}

/// The connection a capture opens on the bundled sample catalog.
const SAMPLE_CONNECTION: Uuid = Uuid::from_u128(0x5a3b_1e00_0000_4000_8000_0000_0000_0001);
/// Two example remotes, listed disconnected beside the sample.
const ANALYTICS_CONNECTION: Uuid = Uuid::from_u128(0x5a3b_1e00_0000_4000_8000_0000_0000_0002);
const SHOP_CONNECTION: Uuid = Uuid::from_u128(0x5a3b_1e00_0000_4000_8000_0000_0000_0003);

/// The connections a populated capture lists: the bundled sample, then two
/// remotes that are not reachable from a screenshot.
fn sample_connections() -> Vec<ConnectionProfile> {
    let mut sample = ConnectionProfile::sqlite_sample();
    sample.id = SAMPLE_CONNECTION;
    let mut analytics = ConnectionProfile::new("Analytics · prod", Engine::Postgres);
    analytics.id = ANALYTICS_CONNECTION;
    analytics.host = "db.internal".into();
    analytics.database = "analytics".into();
    analytics.user = "reader".into();
    let mut shop = ConnectionProfile::new("Shop · staging", Engine::MySql);
    shop.id = SHOP_CONNECTION;
    shop.host = "mysql.staging".into();
    shop.database = "shop".into();
    shop.user = "app".into();
    vec![sample, analytics, shop]
}

/// Framework app: the database client widget over [`DbClient`].
pub struct DbClientShell;

impl ResourceApp for DbClientShell {
    type Resource = DbClient;
    const ID: &'static str = "so.tilo.db-client";
    const TITLE: &'static str = "Database Client";
    const ICON: Option<&'static str> = Some("db-client");

    fn view(cx: &mut ViewCx<DbClient>) -> Element {
        cx.surface(DbClientApp::new())
    }

    /// `empty` is the first run; every other scenario shows the bundled
    /// sample catalog with a query run against it, and `status`, `connect`
    /// and `confirm` add the status bubble, the new-connection sheet and the
    /// destructive-statement confirmation.
    fn scenarios() -> &'static [&'static str] {
        &["default", "empty", "status", "connect", "confirm"]
    }

    fn shot_viewport(_scenario: Option<&str>) -> (u32, u32) {
        (1280, 800)
    }

    fn fixture(fixture: &mut Fixture<DbClient>) {
        if fixture.scenario() != Some("empty") {
            fixture.provide(SampleDatabase {
                connection: SAMPLE_CONNECTION,
            });
        }
    }

    fn scenario(name: Option<&str>) -> Vec<Invoke<DbClient>> {
        if name == Some("empty") {
            return Vec::new();
        }
        sample_connections()
            .iter()
            .map(crate::actions::add_connection_action)
            .chain([DbAction::EditSql(TextEdit::ReplaceText {
                text: crate::sample::DEFAULT_QUERY.to_owned(),
            })])
            .filter_map(|action| Invoke::apply(&action).ok())
            .collect()
    }
}

impl ResourceSurface<DbClient, DbEffect> for DbClientApp {
    fn present(&mut self, state: &Rc<DbState>) {
        self.adopt_resource_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<DbClient>) {
        self.attach_framework(&cx);
    }

    fn stage(&mut self, scenario: Option<&str>) {
        self.stage_scenario(scenario);
    }

    fn effect(&mut self, effect: DbEffect, cx: &mut EffectCx<DbClient, Self>) -> EffectAnswer {
        match effect {
            DbEffect::Query { sql } => self.start_query_effect(&sql, cx),
        }
    }

    fn effects(&self) -> Vec<EffectStatus> {
        if self.session_connected() {
            Vec::new()
        } else {
            vec![EffectStatus::unavailable(
                DbEffect::QUERY,
                "not_connected",
                "Open a saved connection before running SQL",
            )]
        }
    }

    fn live_rows(&self) -> Vec<LiveRow> {
        self.live_rows()
    }

    fn live_row(&self, id: &str) -> Option<LiveRow> {
        self.live_row(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::framework::testing::TestApp;

    #[test]
    fn the_workspace_publishes_its_items_with_their_actions_and_live_rows_beside_them() {
        let mut app = TestApp::<DbClientShell>::new().unwrap();
        let mut profile = ConnectionProfile::new("Analytics", Engine::Postgres);
        profile.host = "db.internal".into();
        profile.password = "s3cret".into();
        app.invoke(Invoke::apply(&crate::actions::add_connection_action(&profile)).unwrap())
            .unwrap();
        app.invoke(
            Invoke::apply(&crate::actions::save_query_action(
                Uuid::new_v4(),
                "Daily totals",
                "SELECT 1",
            ))
            .unwrap(),
        )
        .unwrap();
        app.settle();

        let surface = app.agent_surface().to_string();
        for needle in [
            "add_connection",
            "save_query",
            "rename_connection",
            "remove_connection",
            "remove_query",
            "connect",
            "\"query\"",
            "Run SQL",
            &connection_resource_id(profile.id),
        ] {
            assert!(surface.contains(needle), "missing {needle} in {surface}");
        }
        assert!(
            !surface.contains("database.query-result"),
            "no query ran, so no live result is published"
        );
        assert!(
            !surface.contains("s3cret"),
            "the published tree never holds a password"
        );
    }
}
