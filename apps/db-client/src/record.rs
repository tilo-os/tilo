//! What the shared workspace stores: the saved connections, the saved
//! queries and the SQL buffer.
//!
//! `#[derive(Model)]` is the whole storage contract. The SQL buffer is text a
//! second peer types into character by character; saved connections and
//! queries are items with stable ids. A connection's **password is never
//! stored here**: it stays in each device's keyring
//! ([`crate::model::ConnectionProfile::password`] is `#[serde(skip)]` and
//! [`SavedConnection`] has no field for it), so the record carries the shape
//! of a team's databases without ever replicating a secret.
//!
//! "Which connection is active", "which result tab is showing", "which table
//! is selected", the caret — per-peer view state — stay in the window.

use ui::tilo_resource::{self, Atom, Collection, Id, Model};
use uuid::Uuid;

use crate::model::{ConnectionProfile, Engine};

/// A saved connection profile, without its password.
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
    /// The record of `profile`, without its password.
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

    /// The profile the window opens as `id`, with no password and not
    /// connected.
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

/// A named, collaborative SQL snippet.
#[derive(Model, Clone, Debug, Default, PartialEq, Eq)]
pub struct SavedQuery {
    /// Name: what search finds it by. Saved SQL can hold credentials or
    /// tokens, so it is never found by its text.
    #[model(label)]
    pub name: String,
    /// SQL
    pub sql: String,
}

/// The shared database workspace.
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
    /// The saved connections as the window opens them.
    pub fn profiles(&self) -> Vec<ConnectionProfile> {
        self.connections
            .iter()
            .map(|(id, saved)| saved.profile(id))
            .collect()
    }

    /// The saved connection the window knows as `connection`.
    pub fn connection_id(&self, connection: Uuid) -> Option<Id> {
        self.connections
            .ids()
            .find(|id| uuid_of(id) == connection)
            .cloned()
    }

    /// The saved query the window knows as `query`.
    pub fn saved_query(&self, query: Uuid) -> Option<&SavedQuery> {
        self.saved_queries
            .iter()
            .find(|(id, _)| uuid_of(id) == query)
            .map(|(_, saved)| saved)
    }
}

/// The UUID the window knows the item `id` by: the id itself, which the
/// workspace's verbs and the window mint as UUIDs, or one derived from it
/// for an id minted otherwise.
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
pub fn id_of(uuid: Uuid) -> Id {
    Id::from_raw(uuid.to_string())
}

/// Adding `profile` as the saved connection at `at`, under the id the
/// window knows it by.
#[cfg(test)]
pub(crate) fn adding(
    profile: &ConnectionProfile,
    at: usize,
) -> ui::tilo_resource::Invoke<Workspace> {
    ui::tilo_resource::Invoke::edit(&Workspace::paths().connections.insert_as(
        &id_of(profile.id),
        at,
        &SavedConnection::of(profile),
    ))
    .expect("a connection is an edit")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A profile goes through the record without its password, and an id
    /// that is not a UUID still names one item.
    #[test]
    fn a_profile_round_trips_without_its_password() {
        let mut profile = ConnectionProfile::new("Analytics", Engine::Postgres);
        profile.host = "db.internal".into();
        profile.port = 6543;
        profile.database = "analytics".into();
        profile.user = "reader".into();
        profile.password = "s3cret".into();
        let id = id_of(profile.id);
        let back = SavedConnection::of(&profile).profile(&id);
        assert_eq!(back.id, profile.id);
        assert_eq!(
            (
                back.name.as_str(),
                back.host.as_str(),
                back.port,
                back.user.as_str()
            ),
            ("Analytics", "db.internal", 6543, "reader")
        );
        assert!(
            back.password.is_empty(),
            "the password is not in the record"
        );
        let foreign = Id::from_raw("conn-1");
        assert_eq!(uuid_of(&foreign), uuid_of(&foreign));
        assert_ne!(uuid_of(&foreign), uuid_of(&Id::from_raw("conn-2")));
    }
}
