//! What a workspace keeps beside its records on the host that serves it, as the actions each
//! answers and what they read as: its vault ([`vault`]), its connected mail and calendar
//! accounts ([`accounts`]), its model providers ([`models`]) and the services it connected
//! ([`connections`]). Each is declared here once; the workspace service keeps them and answers
//! their actions, and every crate that calls them binds to these declarations.

/// The workspace's connected accounts: their actions, declared once, and what they read as.
pub mod accounts;
/// The services a workspace connected: their actions, declared once, and what they read as.
pub mod connections;
/// The workspace's model providers: their actions, declared once, and what they read as.
pub mod models;
/// The workspace's vault: its actions, declared once, and what they read as.
pub mod vault;
