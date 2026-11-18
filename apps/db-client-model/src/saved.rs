//! What the shared workspace stores: saved connections, saved queries and the
//! SQL buffer. `#[derive(Model)]` is the storage contract. A connection's
//! password is never stored here: it stays in each device's keyring, so the
//! resource carries the shape of a team's databases without a secret.

use tilo_resource::{Atom, Collection, Id, Model};
use uuid::Uuid;

use crate::profile::{ConnectionProfile, Engine};

#[derive(Model, Clone, Debug, Default, PartialEq, Eq)]
pub struct SavedConnection {
    /// Name
    #[model(label)]
    pub name: String,
    /// Engine
    pub engine: Engine,
    /// Host, for a network engine.
    pub host: Atom,
    /// Port, for a network engine.
    pub port: u16,
    /// Database
    pub database: Atom,
    /// User
    pub user: Atom,
    /// File, for `SQLite`.
    pub file_path: Atom,
}

impl SavedConnection {
    #[must_use]
    pub fn of(profile: &ConnectionProfile) -> Self {
        Self {
            name: profile.name.clone(),
            engine: profile.engine,
            host: Atom(profile.host.clone()),
            port: profile.port,
            database: Atom(profile.database.clone()),
            user: Atom(profile.user.clone()),
            file_path: Atom(profile.file_path.clone()),
        }
    }

    #[must_use]
    pub fn profile(&self, id: &Id) -> ConnectionProfile {
        ConnectionProfile {
            id: uuid_of(id),
            name: self.name.clone(),
            engine: self.engine,
            host: self.host.0.clone(),
            port: self.port,
            database: self.database.0.clone(),
            user: self.user.0.clone(),
            password: String::new(),
            file_path: self.file_path.0.clone(),
            connected: false,
            is_sample: false,
        }
    }
}

#[derive(Model, Clone, Debug, Default, PartialEq, Eq)]
pub struct SavedQuery {
    /// The name search finds it by; saved SQL can hold credentials, so its text is never
    /// searched.
    #[model(label)]
    pub name: String,
    /// SQL
    pub sql: String,
}

#[derive(Model, Clone, Debug, Default, PartialEq)]
pub struct Workspace {
    /// Saved connections
    pub connections: Collection<SavedConnection>,
    /// Saved queries
    pub saved_queries: Collection<SavedQuery>,
    /// SQL: the query buffer, edited a splice at a time.
    pub sql: String,
}

impl Workspace {
    #[must_use]
    pub fn profiles(&self) -> Vec<ConnectionProfile> {
        self.connections
            .iter()
            .map(|(id, saved)| saved.profile(id))
            .collect()
    }

    #[must_use]
    pub fn connection_id(&self, connection: Uuid) -> Option<Id> {
        self.connections
            .ids()
            .find(|id| uuid_of(id) == connection)
            .cloned()
    }

    #[must_use]
    pub fn saved_query(&self, query: Uuid) -> Option<&SavedQuery> {
        self.saved_queries
            .iter()
            .find(|(id, _)| uuid_of(id) == query)
            .map(|(_, saved)| saved)
    }
}

/// The UUID the window knows the item `id` by, or one derived from an id minted
/// otherwise.
#[must_use]
pub fn uuid_of(id: &Id) -> Uuid {
    Uuid::parse_str(id.as_str()).unwrap_or_else(|_| {
        let half = |salt: u8| {
            use std::hash::{Hash, Hasher};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            salt.hash(&mut hasher);
            id.as_str().hash(&mut hasher);
            hasher.finish()
        };
        Uuid::from_u64_pair(half(0), half(1))
    })
}

/// The id of the item the window knows as `uuid`.
#[must_use]
pub fn id_of(uuid: Uuid) -> Id {
    Id::from_raw(uuid.to_string())
}
