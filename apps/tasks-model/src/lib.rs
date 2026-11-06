//! The `tasks.tracker` record, as the app that draws it and the agent that works its issues
//! both read it.
//!
//! It is also the model module the document host runs (`docs/architecture/apps.md` §1): the
//! kind below is what the host serves over the record's address, and [`module`] is the export
//! it calls. That export, and the host imports under it, exist only when the crate is built as
//! the module (`--features module`): a browser app links this crate for its types, and a wasm
//! build that carried the module's `tilo_model` imports would have nothing to resolve them
//! with.

pub mod model;
#[cfg(feature = "module")]
mod module;

pub use model::*;
