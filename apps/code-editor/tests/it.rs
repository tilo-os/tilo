#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failed unwrap or index in a test is its assertion"
)]
#[path = "history_and_agents.rs"]
pub mod history_and_agents;
#[path = "language_server.rs"]
pub mod language_server;
#[path = "open_folder.rs"]
pub mod open_folder;
