//! The engines a connection targets, and the profile the window holds of one
//! while it is open: the resource keeps its shape, never its password.

use serde::{Deserialize, Serialize};
use tilo_resource::Model;
use uuid::Uuid;

/// Which engine a connection targets. `SQLite` is local and always available; Postgres and
/// `MySQL` are network engines.
#[derive(Model, Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Engine {
    Sqlite,
    #[default]
    Postgres,
    #[model(rename = "mysql")]
    #[serde(rename = "mysql")]
    MySql,
}

impl Engine {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Engine::Sqlite => "SQLite",
            Engine::Postgres => "PostgreSQL",
            Engine::MySql => "MySQL",
        }
    }

    #[must_use]
    pub fn default_port(self) -> u16 {
        match self {
            Engine::Sqlite => 0,
            Engine::Postgres => 5432,
            Engine::MySql => 3306,
        }
    }

    #[must_use]
    pub fn all() -> [Engine; 3] {
        [Engine::Sqlite, Engine::Postgres, Engine::MySql]
    }

    /// The stable string an engine is named by where it is typed, apart from its label, so
    /// the key never shifts with a UI rename.
    #[must_use]
    pub fn as_key(self) -> &'static str {
        match self {
            Engine::Sqlite => "sqlite",
            Engine::Postgres => "postgres",
            Engine::MySql => "mysql",
        }
    }

    /// An engine from its key; an unknown key reads as `PostgreSQL`, so a
    /// forward-compatible document still renders.
    #[must_use]
    pub fn from_key(key: &str) -> Engine {
        match key {
            "sqlite" => Engine::Sqlite,
            "mysql" => Engine::MySql,
            _ => Engine::Postgres,
        }
    }
}

/// A connection as the window holds it while it is open, with the password it
/// never puts in the resource.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConnectionProfile {
    pub id: Uuid,
    pub name: String,
    pub engine: Engine,
    pub host: String,
    pub port: u16,
    pub database: String,
    pub user: String,
    /// The password, held in memory only: `#[serde(skip)]` keeps plaintext out of the
    /// profile on disk.
    #[serde(skip, default)]
    pub password: String,
    pub file_path: String,
    #[serde(default)]
    pub connected: bool,
    /// True for the read-only sample database that ships with the app, so the browser and
    /// query runner work offline.
    #[serde(default)]
    pub is_sample: bool,
}

impl ConnectionProfile {
    #[cfg(feature = "samples")]
    #[must_use]
    pub fn sqlite_sample() -> Self {
        Self {
            id: Uuid::new_v4(),
            name: "Chinook sample".to_string(),
            engine: Engine::Sqlite,
            host: String::new(),
            port: 0,
            database: "main".to_string(),
            file_path: ":memory:".to_string(),
            user: String::new(),
            password: String::new(),
            connected: true,
            is_sample: true,
        }
    }

    #[must_use]
    pub fn new(name: &str, engine: Engine) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.to_string(),
            engine,
            host: "localhost".to_string(),
            port: engine.default_port(),
            database: String::new(),
            user: String::new(),
            password: String::new(),
            file_path: String::new(),
            connected: false,
            is_sample: false,
        }
    }

    #[must_use]
    pub fn subtitle(&self) -> String {
        match self.engine {
            Engine::Sqlite => {
                if self.file_path == ":memory:" {
                    "in-memory".to_string()
                } else if self.file_path.is_empty() {
                    "no file".to_string()
                } else {
                    self.file_path.clone()
                }
            }
            _ => format!("{}:{}/{}", self.host, self.port, self.database),
        }
    }

    #[must_use]
    pub fn address(&self) -> String {
        match self.engine {
            Engine::Sqlite if self.is_sample || self.file_path == ":memory:" => String::new(),
            Engine::Sqlite => std::path::Path::new(&self.file_path)
                .file_name()
                .map_or_else(
                    || self.file_path.clone(),
                    |name| name.to_string_lossy().into_owned(),
                ),
            Engine::Postgres | Engine::MySql => format!("{}:{}", self.host, self.port),
        }
    }

    /// Whether a sandbox grant is needed before a live handle opens.
    #[must_use]
    pub fn is_remote(&self) -> bool {
        !matches!(self.engine, Engine::Sqlite)
    }
}
