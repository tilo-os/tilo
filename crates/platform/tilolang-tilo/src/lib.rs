//! Tilo platform integration for tilolang apps.
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
//! * [`instance`]: one window of an app as an app host runs it, without the toolkit, sending
//!   its window component patches and taking what the person did;

pub mod action;
pub mod authority;
pub mod background;
pub mod bench;
pub mod bundle;
pub mod effects;
pub mod host;
pub mod index;
pub mod instance;
pub mod items;
pub mod meta;
pub mod origin;
pub mod publish;
pub mod reading;
pub mod reads;
pub mod record;
pub mod schema;
pub mod search;
pub mod sink;
pub mod store;
pub mod surface;
pub mod upgrade;

pub use action::{Authorship, DynAction, DynEdit, register_actions};
pub use effects::{HostEffect, OpenEffect, OpenPlan};
pub use meta::MetaExt;
pub use record::{DynRecord, DynValue};
pub use schema::{FieldSchema, MergeKind, TypeSchema};
/// The compiler, for hosts that build packages from source.
pub use tilolang;
pub use tilolang::HOST_APP;
/// The runtime, for hosts that drive views and read values.
pub use tilolang_runtime as runtime;
