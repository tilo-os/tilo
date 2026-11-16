//! The record store's documents: their fields, how a record that left its
//! workspace is retired, and how each is saved. A record's identity is
//! `collab_types::record`.

pub mod fields;
pub mod retirement;
pub mod save;
pub use save::RecordSaveStatus;
