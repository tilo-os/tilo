//! Tilo platform integration for tilolang apps (implementation design section 7).
//!
//! The tilolang compiler and runtime know nothing of Tilo. This crate is the seam to the
//! platform:
//!
//! * [`record`]: `DynRecord`, a tilolang resource as a `tilo_resource` model, so resources live
//!   in the room record and merge by their field kinds;
//! * [`action`]: `DynAction`, the replayable form of a planned tilolang action, which a peer
//!   without the wasm module applies;
//! * [`meta`] and [`surface`]: what the platform derives from package metadata, and the surface
//!   generated from it;
//! * [`publish`]: the resources and typed actions published on the action door;

pub mod action;
pub mod authority;
pub mod host;
pub mod meta;
pub mod publish;
pub mod record;
pub mod schema;
pub mod store;
pub mod surface;

pub use action::{Authorship, DynAction, DynEdit, register_actions};
pub use meta::MetaExt;
pub use record::{DynRecord, DynValue};
pub use schema::{FieldSchema, MergeKind, TypeSchema};
/// The runtime, for hosts that drive views and read values.
pub use tilolang_runtime as runtime;
