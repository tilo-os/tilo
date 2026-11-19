//! The `db-client.workspace` resource: a workspace's connection profiles, its
//! saved queries and the SQL buffer, as the app that draws them and an agent
//! that works them both read it.
//!
//! A connection's password is never part of the resource: it stays in each
//! device's keyring, so the resource carries the shape of a team's databases
//! without a secret. Everything done on a server is an effect, offered on the
//! saved connection or query it acts on.

mod profile;
mod resource;
pub mod rows;
mod saved;

pub use profile::{ConnectionProfile, Engine};
pub use resource::DbEffect;
pub use resource::workspace;
pub use saved::{SavedConnection, SavedQuery, Workspace, id_of, uuid_of};
