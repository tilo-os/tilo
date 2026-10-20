//! Tilo platform integration for tilolang apps (implementation design section 7).
//!
//! The tilolang compiler and runtime know nothing of Tilo. This crate is the seam to the
//! platform:
//!
//! * [`record`]: `DynRecord`, a tilolang resource as a `tilo_resource` model, so resources live
//!   in the room record and merge by their field kinds;
//! * [`action`]: `DynAction`, the replayable form of a planned tilolang action, which a peer
//!   without the wasm module applies;
//! * [`meta`] and [`surface`]: the package metadata and the surface generated from it;
//! * [`publish`]: the resources and typed actions published on the action door;
//! * [`ui`]: the runtime's view tree and patches as a UI graph.

pub mod action;
pub mod meta;
pub mod publish;
pub mod record;
pub mod schema;
pub mod surface;
pub mod ui;

pub use action::{Authorship, DynAction, DynEdit, register_actions};
pub use meta::PackageMeta;
pub use record::{DynRecord, DynValue};
pub use schema::{FieldSchema, MergeKind, TypeSchema};
