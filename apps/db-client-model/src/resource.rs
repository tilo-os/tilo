//! The resource's verbs: connection profiles and saved queries, and their
//! passwords staying on the device. Field edits at their paths change names and
//! SQL; adding and removing are verbs. Everything done on a server is a
//! [`DbEffect`], offered on the saved connection or query it acts on.

use tilo_resource::items::item_resource_id;
use tilo_resource::{self, ActionCx, Effect, Id, NonBlank, Refusal, SearchEntry, resource};
use uuid::Uuid;

use crate::profile::Engine;
use crate::saved::{SavedConnection, SavedQuery, Workspace, id_of};

impl Workspace {
    /// What the resource holds: the connection profiles, the saved queries and the SQL
    /// buffer.
    ///
    /// # Errors
    /// When the fields cannot be read from the document.
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
                return Err(Refusal::new(format!("There is no engine called {other}.")));
            }
        };
        let mut profile = crate::profile::ConnectionProfile::new(name.trim(), engine);
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
        let queries =
            self.saved_queries.iter().map(|(id, saved)| {
                SearchEntry::new(&saved.name, "Collaborative saved query")
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
    /// Execute SQL
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
    /// Execute a historical query
    #[effect(row, job, destructive)]
    Rerun { connection: Uuid, generation: usize },
}
