//! The `tasks.tracker` record, as the app that draws it and the agent that works its issues
//! both read it.
//!
//! It is also the model module the document host runs (`docs/architecture/apps.md` §1): the
//! kind below is what the host serves over the record's address, and [`module`] is the export
//! it calls.

pub mod model;
mod module;

pub use model::*;
