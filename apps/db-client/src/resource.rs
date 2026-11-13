//! The record's verbs: connection profiles and saved queries sync, and their passwords stay
//! on the device. Typing SQL and renaming are field edits at their paths; adding and
//! removing are verbs. Everything done on a server is a [`DbEffect`], offered on the saved
//! connection or query it acts on.

#[cfg(feature = "samples")]
use ui::framework::Fixture;
use ui::framework::{ResourceApp, ViewCx};
use ui::i18n::EmbeddedCatalog;
#[cfg(feature = "samples")]
use ui::tilo_resource::Invoke;
use ui::tilo_resource::items::item_resource_id;
use ui::tilo_resource::{self, ActionCx, Effect, Id, NonBlank, Refusal, SearchEntry, resource};
use ui::widget::Element;
use uuid::Uuid;

use crate::app::DbClientApp;
use crate::model::{ConnectionProfile, Engine};
use crate::record::{SavedConnection, SavedQuery, id_of};

pub use crate::record::Workspace;

impl Workspace {
    /// What the record holds: the connection profiles, the saved queries and the SQL
    /// buffer.
    pub fn read_document(
        &self,
        item: Option<&tilo_resource::Id>,
    ) -> Result<serde_json::Value, String> {
        tilo_resource::read::fields(self, item)
    }
}

#[resource(
    kind = "db-client.workspace",
    search = Workspace::search,
    effects = DbEffect,
    read_content = Workspace::read_document
)]
impl Workspace {
    /// Add connection
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
            other => {
                return Err(Refusal::new(crate::i18n::t_args(
                    "db-unknown-engine",
                    &[("engine", crate::i18n::text(other))],
                )));
            }
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
    #[action(destructive, irreversible, item = connections)]
    pub fn remove_connection(&mut self, connection: &Id) -> Result<(), Refusal> {
        self.connections
            .remove(connection)
            .map(drop)
            .ok_or_else(|| Refusal::new("no such connection"))
    }

    /// Save query
    #[action]
    pub fn save_query(
        &mut self,
        cx: &mut ActionCx,
        #[param("Query name")] name: String,
        #[param("SQL; the query being written when none")] sql: Option<NonBlank>,
    ) -> Result<(), Refusal> {
        let sql = match sql {
            Some(sql) => sql.into_string(),
            None if self.sql.trim().is_empty() => {
                return Err(Refusal::new("there is no SQL to save"));
            }
            None => self.sql.clone(),
        };
        self.saved_queries.push(
            id_of(cx.uuid()),
            SavedQuery {
                name: name.trim().to_owned(),
                sql,
            },
        );
        Ok(())
    }

    #[action(destructive, item = saved_queries)]
    pub fn remove_query(&mut self, query: &Id) -> Result<(), Refusal> {
        self.saved_queries
            .remove(query)
            .map(drop)
            .ok_or_else(|| Refusal::new("no such query"))
    }
}

impl Workspace {
    /// What search finds: connections by name, engine and host, saved queries by name
    /// only, each anchored at the item it opens.
    fn search(&self) -> Vec<SearchEntry> {
        let connections = self.connections.iter().map(|(id, saved)| {
            SearchEntry::new(
                &saved.name,
                format!("{} {}", saved.engine.as_key(), saved.host.0),
            )
            .anchored(item_resource_id::<Self>("connections", id))
        });
        let queries = self.saved_queries.iter().map(|(id, saved)| {
            SearchEntry::new(&saved.name, crate::i18n::t("db-search-saved-query"))
                .anchored(item_resource_id::<Self>("saved_queries", id))
        });
        connections.chain(queries).collect()
    }
}

/// What the client does on a server, which changes no document. Each effect names the
/// catalog entry, result or history entry it acts on by id, and resolves it again when
/// invoked. None carries a password.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "db_client.effects")]
pub enum DbEffect {
    /// Run SQL
    #[effect(job, cancellable, destructive)]
    Query {
        #[param("SQL query")]
        sql: String,
    },
    /// Connect
    #[effect(row, job, idempotent, item = connections)]
    Connect {
        #[param("The saved connection")]
        connection: String,
    },
    #[effect(row, job, destructive, item = saved_queries)]
    Run {
        #[param("The saved query")]
        query: String,
    },
    /// New connection
    #[effect(view)]
    NewConnection,
    #[effect(view, row)]
    FocusDatabase { connection: Uuid },
    #[effect(view, row)]
    FocusSchema { connection: Uuid, schema: String },
    #[effect(row, job, idempotent)]
    Browse {
        connection: Uuid,
        schema: String,
        table: String,
    },
    #[effect(view, row)]
    ShowResult { connection: Uuid, generation: usize },
    /// Read query result
    #[effect(view, row, returns)]
    ReadResult { connection: Uuid, generation: usize },
    #[effect(attended, row, job)]
    ExportCsv { connection: Uuid, generation: usize },
    #[effect(row, job)]
    Begin { connection: Uuid },
    #[effect(row, job, destructive)]
    Commit { connection: Uuid },
    #[effect(row, job, destructive)]
    Rollback { connection: Uuid },
    /// Run historical query
    #[effect(row, job, destructive)]
    Rerun { connection: Uuid, generation: usize },
}

#[cfg(feature = "samples")]
pub(crate) struct SampleDatabase {
    pub(crate) connection: Uuid,
}

#[cfg(feature = "samples")]
const SAMPLE_CONNECTION: Uuid = Uuid::from_u128(0x5a3b_1e00_0000_4000_8000_0000_0000_0001);
#[cfg(feature = "samples")]
const ANALYTICS_CONNECTION: Uuid = Uuid::from_u128(0x5a3b_1e00_0000_4000_8000_0000_0000_0002);
#[cfg(feature = "samples")]
const SHOP_CONNECTION: Uuid = Uuid::from_u128(0x5a3b_1e00_0000_4000_8000_0000_0000_0003);

#[cfg(feature = "samples")]
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

pub struct DbClientShell;

impl ResourceApp for DbClientShell {
    type Resource = Workspace;
    const ID: &'static str = "so.tilo.db-client";
    const TITLE: &'static str = "Database Client";
    const TITLE_KEY: &'static str = "db-client-app-title";
    const ICON: Option<&'static str> = Some("db-client");

    fn view(cx: &mut ViewCx<Workspace>) -> Element {
        let app = DbClientApp::with_actions(&cx.actions());
        let (opener, index) = {
            let state = app.state();
            (state.opener(), state.search_index())
        };
        // A search hit, or an agent's `open`, shows the item it names.
        cx.on_open_item(opener);
        // A catalogue hit — a relation, column, result, transaction or query
        // of the open connection — runs what its row offers.
        let dispatcher = cx.dispatcher();
        cx.on_open(
            move |anchor: &str| -> Result<(), ui::tilo_resource::Refusal> {
                let effect = index.opened(anchor).ok_or_else(|| {
                    ui::tilo_resource::Refusal::new(format!(
                        "`{anchor}` is not in the open catalogue"
                    ))
                })?;
                dispatcher.effect(&effect);
                Ok(())
            },
        );
        crate::app::window(cx, app)
    }

    fn catalog() -> Option<&'static EmbeddedCatalog> {
        Some(crate::i18n::catalogs())
    }

    /// `empty` is the first run; every other scenario shows the bundled
    /// sample catalog with a query run against it, on the tab or dialog it
    /// names (see `Store::stage_scenario` in the app).
    #[cfg(feature = "samples")]
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

    #[cfg(feature = "samples")]
    fn fixture(fixture: &mut Fixture<Workspace>) {
        if fixture.scenario() != Some("empty") {
            fixture.provide(SampleDatabase {
                connection: SAMPLE_CONNECTION,
            });
        }
    }

    #[cfg(feature = "samples")]
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

#[cfg(test)]
mod tests;

ui::reach_tests!(DbClientShell);

#[cfg(test)]
mod agent_disclosure_tests {
    use ui::framework::ResourceApp;
    use ui::framework::testing::TestApp;
    use ui::tilo_actions::ResourceSpec;

    /// The document read of the first resource of `kind` any scenario
    /// publishes, invoked as `tilo_read` on it does.
    fn read_first(kind: &str) -> ui::serde_json::Value {
        for scenario in <super::DbClientShell as ResourceApp>::scenarios() {
            let mut app = TestApp::<super::DbClientShell>::drive(scenario)
                .unwrap_or_else(|error| panic!("{scenario}: {error}"));
            app.settle();
            let resources = app.harness().resources();
            if let Some((resource, read)) = document_read(&resources, kind) {
                return app
                    .call_on(&resource, &read, ui::serde_json::json!({}))
                    .unwrap_or_else(|error| panic!("{kind} in {scenario}: {error}"))
                    .unwrap_or_else(|| panic!("the read of {kind} answers"));
            }
        }
        panic!("no scenario publishes a {kind} with a document read");
    }

    fn document_read(resources: &[ResourceSpec], kind: &str) -> Option<(String, String)> {
        resources.iter().find_map(|resource| {
            resource
                .actions
                .iter()
                .find(|action| resource.kind == kind && action.contract.reads_document())
                .map(|read| (resource.id.as_str().to_owned(), read.id.clone()))
                .or_else(|| document_read(&resource.children, kind))
        })
    }

    #[test]
    fn a_query_result_is_read_through_its_document_read() {
        let answer = read_first("database.query-result");
        assert!(answer["message"].is_string(), "{answer}");
        assert!(answer["returns_rows"].is_boolean(), "{answer}");
    }

    #[test]
    fn every_scenario_shows_an_agent_its_actions() {
        if let Err(failures) =
            ui::framework::testing::check_agents_are_shown_every_action::<super::DbClientShell>()
        {
            panic!("{failures}");
        }
    }
}
