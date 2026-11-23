//! Tilo platform integration for tilolang apps.
//!
//! The tilolang compiler and runtime know nothing of Tilo. This crate is the boundary to the
//! platform:
//!
//! * [`resources`]: the runtime's store over the device's one resource store, where every
//!   resource of a tilolang app is kept;
//! * [`action`]: `DynAction`, the record of an invoked tilolang action;
//! * [`meta`] and [`surface`]: what the platform derives from package metadata, and the surface
//!   generated from it;
//! * [`publish`]: the resources and typed actions published on the action api;
//! * [`instance`]: one window of an app as an app host runs it, without the toolkit, sending
//!   its window component patches and taking what the person did;

pub mod action;
pub mod authority;
pub mod background;
pub mod bundle;
pub mod call;
pub mod catalogue;
pub mod effects;
pub mod host;
pub mod instance;
pub mod members;
pub mod meta;
pub mod names;
pub mod native;
pub mod notices;
pub mod origin;
pub mod personal;
pub mod platform_apps;
pub mod publish;
pub mod published;
pub mod reading;

pub mod resource_service;
pub mod resources;
pub mod schema;
pub mod search;
pub mod seed;
pub mod share;
pub mod shell;
pub mod sink;
pub mod surface;
pub mod task;
pub mod task_reads;
pub mod triggers;
pub mod upgrade;
pub mod used;
pub mod views;

pub use action::{Authorship, DynAction, register_actions};
pub use effects::{HostEffect, OpenEffect, OpenPlan};
pub use meta::MetaExt;
pub use schema::{FieldSchema, MergeKind, TypeSchema};
/// The compiler, for hosts that build packages from source.
pub use tilolang;
pub use tilolang::HOST_APP;
/// The runtime, for hosts that drive views and read values.
pub use tilolang_runtime as runtime;
