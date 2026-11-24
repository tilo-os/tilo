//! A document an app holds of its record, and what it knows of who wrote it. The history it
//! keeps is the store's ([`tilo_resource_store::history`]).

mod replica;
pub use replica::{DocumentFrontier, DocumentReplica, State, WritingActors, changes_beyond};
