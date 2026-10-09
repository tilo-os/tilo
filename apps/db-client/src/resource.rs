//! The database client's record and verbs: connection profiles and saved
//! queries sync (passwords stay on the device), and the SQL buffer is shared
//! text. Renaming a connection or a query and typing SQL are the built-in
//! field edits of their paths (`Workspace::paths().sql.splice(..)`);
//! adding and removing them are verbs. Connecting, running SQL and
//! everything else done on a database server are [`DbEffect`]s.

use std::rc::Rc;

use ui::framework::{
    EffectAnswer, EffectCx, EffectStatus, Fixture, LiveRow, ResourceApp, ResourceSurface,
    SurfaceCx, ViewCx,
};
use ui::i18n::EmbeddedCatalog;
use ui::tilo_resource::items::item_resource_id;
use ui::tilo_resource::{self, ActionCx, Effect, Id, Invoke, Refusal, SearchEntry, resource};
use ui::widget::Element;
use uuid::Uuid;

use crate::app::DbClientApp;
use crate::model::{ConnectionProfile, Engine};
use crate::record::{SavedConnection, SavedQuery, id_of};

pub use crate::record::Workspace;

/// The database client's Fluent catalogs, source locale first.
pub const LOCALES: &[(&str, &str)] = &[("en-US", include_str!("../locales/en-US.ftl"))];

static CATALOG: EmbeddedCatalog =
    EmbeddedCatalog::new("en-US", LOCALES, "db-client-add-connection").with_system_locale();

#[resource(kind = "db-client.workspace", label = "Database", search = Workspace::search)]
impl Workspace {
    /// Add connection
    ///
    /// Save a connection profile; its password stays on each device. A
    /// network engine connects on its default port until the port is edited.
    #[action]
    pub fn add_connection(
        &mut self,
        cx: &mut ActionCx,
        #[param("Connection name")] name: String,
        #[param("Database engine: sqlite, postgres or mysql")] engine: String,
        #[param("Server host, or the SQLite file")] location: Option<String>,
        #[param("Database name")] database: Option<String>,
        #[param("User name")] user: Option<String>,
    ) -> Result<(), Refusal> {
        let engine = match engine.as_str() {
            "sqlite" => Engine::Sqlite,
            "postgres" => Engine::Postgres,
            "mysql" => Engine::MySql,
            other => return Err(Refusal::new(format!("no engine is called {other}"))),
        };
        let mut profile = ConnectionProfile::new(name.trim(), engine);
        if let Some(location) = location {
            if engine == Engine::Sqlite {
                profile.file_path = location;
            } else {
                profile.host = location;
            }
        }
        if let Some(database) = database {
            profile.database = database;
        }
        if let Some(user) = user {
            profile.user = user;
        }
        self.connections
            .push(id_of(cx.uuid()), SavedConnection::of(&profile));
        Ok(())
    }

    /// Remove saved connection
    #[action(destructive, item = connections)]
    pub fn remove_connection(&mut self, connection: &Id) -> Result<(), Refusal> {
        self.connections
            .remove(connection)
            .map(drop)
            .ok_or_else(|| Refusal::new("no such connection"))
    }

    /// Save query
    ///
    /// Save SQL under a name: the SQL given, or the query being written.
    #[action]
    pub fn save_query(
        &mut self,
        cx: &mut ActionCx,
        #[param("Query name")] name: String,
        #[param("SQL; the query being written when none")] sql: Option<String>,
    ) -> Result<(), Refusal> {
        let sql = sql.unwrap_or_else(|| self.sql.clone());
        if sql.trim().is_empty() {
            return Err(Refusal::new("there is no SQL to save"));
        }
        self.saved_queries.push(
            id_of(cx.uuid()),
            SavedQuery {
                name: name.trim().to_owned(),
                sql,
            },
        );
        Ok(())
    }

    /// Remove saved query
    #[action(destructive, item = saved_queries)]
    pub fn remove_query(&mut self, query: &Id) -> Result<(), Refusal> {
        self.saved_queries
            .remove(query)
            .map(drop)
            .ok_or_else(|| Refusal::new("no such query"))
    }
}

impl Workspace {
    /// What system search finds: connections by their name, engine and
    /// host, saved queries by their name only (saved SQL can hold
    /// credentials or tokens), each anchored at the item it opens.
    fn search(&self) -> Vec<SearchEntry> {
        let connections = self.connections.iter().map(|(id, saved)| {
            SearchEntry::new(
                &saved.name,
                format!("{} {}", saved.engine.as_key(), saved.host.0),
            )
            .anchored(item_resource_id::<Self>("connections", id))
        });
        let queries =
            self.saved_queries.iter().map(|(id, saved)| {
                SearchEntry::new(&saved.name, "Collaborative saved query")
                    .anchored(item_resource_id::<Self>("saved_queries", id))
            });
        connections.chain(queries).collect()
    }
}

/// The client's work on a database server and what it shows of one, which
/// change no document. Running SQL, connecting and running a saved query are
/// offered on the workspace, naming the connection or query by id; everything
/// else rides the catalog entry, result or history entry it acts on, which it
/// names by id and resolves again when invoked. None carries a password.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "db_client.effects", device_only)]
pub enum DbEffect {
    /// Run SQL
    ///
    /// Runs a statement on the open connection; it may change the database.
    #[effect(job, cancellable, destructive)]
    Query {
        #[param("SQL query")]
        sql: String,
    },
    /// Connect
    ///
    /// Opens the saved connection on this device, with this device's
    /// password.
    #[effect(job, idempotent)]
    Connect { connection: Uuid },
    /// Run saved query
    #[effect(job, destructive)]
    Run { query: Uuid },
    /// Show database
    #[effect(view, row)]
    FocusDatabase { connection: Uuid },
    /// Show schema
    #[effect(view, row)]
    FocusSchema { connection: Uuid, schema: String },
    /// Browse rows
    #[effect(row, job, idempotent)]
    Browse {
        connection: Uuid,
        schema: String,
        table: String,
    },
    /// Show query result
    #[effect(view, row)]
    ShowResult { connection: Uuid, generation: usize },
    /// Export CSV
    #[effect(row, job)]
    ExportCsv { connection: Uuid, generation: usize },
    /// Begin transaction
    #[effect(row, job)]
    Begin { connection: Uuid },
    /// Commit transaction
    #[effect(row, job, destructive)]
    Commit { connection: Uuid },
    /// Roll back transaction
    #[effect(row, job, destructive)]
    Rollback { connection: Uuid },
    /// Run historical query
    ///
    /// Runs again a query from this device's own history, which is never
    /// shared.
    #[effect(row, job, destructive)]
    Rerun { connection: Uuid, generation: usize },
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
    let mut analytics = ConnectionProfile::new("Production analytics", Engine::Postgres);
    analytics.id = ANALYTICS_CONNECTION;
    analytics.host = "db.internal".into();
    analytics.database = "analytics".into();
    analytics.user = "reader".into();
    let mut shop = ConnectionProfile::new("Staging shop", Engine::MySql);
    shop.id = SHOP_CONNECTION;
    shop.host = "mysql.staging".into();
    shop.database = "shop".into();
    shop.user = "app".into();
    vec![sample, analytics, shop]
}

/// Framework app: the database client widget over [`Workspace`].
pub struct DbClientShell;

impl ResourceApp for DbClientShell {
    type Resource = Workspace;
    const ID: &'static str = "so.tilo.db-client";
    const TITLE: &'static str = "Database Client";
    const ICON: Option<&'static str> = Some("db-client");

    fn view(cx: &mut ViewCx<Workspace>) -> Element {
        let app = DbClientApp::new();
        // A search hit, or an agent's `open`, shows the item it names.
        cx.on_open_item(app.opener());
        cx.surface(app)
    }

    fn catalog() -> Option<&'static EmbeddedCatalog> {
        Some(&CATALOG)
    }

    /// `empty` is the first run; every other scenario shows the bundled
    /// sample catalog with a query run against it, on the tab or dialog it
    /// names (see [`DbClientApp::stage_scenario`]).
    fn scenarios() -> &'static [&'static str] {
        &[
            "data",
            "structure",
            "wide",
            "connect",
            "confirm",
            "error",
            "empty",
        ]
    }

    fn shot_viewport(_scenario: Option<&str>) -> (u32, u32) {
        (1280, 800)
    }

    fn fixture(fixture: &mut Fixture<Workspace>) {
        if fixture.scenario() != Some("empty") {
            fixture.provide(SampleDatabase {
                connection: SAMPLE_CONNECTION,
            });
        }
    }

    fn scenario(name: Option<&str>) -> Vec<Invoke<Workspace>> {
        if name == Some("empty") {
            return Vec::new();
        }
        let paths = Workspace::paths();
        sample_connections()
            .iter()
            .enumerate()
            .map(|(at, profile)| {
                Invoke::edit(&paths.connections.insert_as(
                    &id_of(profile.id),
                    at,
                    &SavedConnection::of(profile),
                ))
            })
            .chain([Invoke::edit(
                &paths.sql.replace(crate::app::scenario_query(name)),
            )])
            .filter_map(Result::ok)
            .collect()
    }
}

impl ResourceSurface<Workspace, DbEffect> for DbClientApp {
    fn present(&mut self, state: &Rc<Workspace>) {
        self.adopt_resource_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<Workspace>) {
        self.attach_framework(&cx);
    }

    fn stage(&mut self, scenario: Option<&str>) {
        self.stage_scenario(scenario);
    }

    fn effect(&mut self, effect: DbEffect, cx: &mut EffectCx<Workspace, Self>) -> EffectAnswer {
        self.answer_effect(effect, cx)
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

    /// The open connection's relations, columns, result, transaction and
    /// local query history: what the live catalog holds, not the document.
    fn search_entries(&self) -> Vec<SearchEntry> {
        self.catalog_search_entries()
    }
}

#[cfg(test)]
mod tests;
