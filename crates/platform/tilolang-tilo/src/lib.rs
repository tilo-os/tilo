//! Tilo platform integration for tilolang apps.
//!
//! The tilolang compiler and runtime know nothing of Tilo. This crate is the seam to the
//! platform:
//!
//! * [`record`]: `DynRecord`, a tilolang resource as a `tilo_resource` model, so resources live
//!   in the workspace record and merge by their field kinds;
//! * [`action`]: `DynAction`, the replayable form of a planned tilolang action, which a peer
//!   without the wasm module applies;
//! * [`meta`] and [`surface`]: what the platform derives from package metadata, and the surface
//!   generated from it;
//! * [`publish`]: the resources and typed actions published on the action api;
//! * [`instance`]: one window of an app as an app host runs it, without the toolkit, sending
//!   its window component patches and taking what the person did;

pub mod action;
pub mod authority;
pub mod background;
pub mod bench;
pub mod bundle;
pub mod catalogue;
pub mod chunk;
pub mod containers;
pub mod content;
pub mod effects;
pub mod history;
pub mod host;
pub mod index;
pub mod instance;
pub mod items;
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
pub mod reads;
pub mod record;
pub mod records_service;
pub mod schema;
pub mod search;
pub mod sink;
pub mod store;
pub mod surface;
pub mod task;
pub mod task_records;
pub mod triggers;
pub mod upgrade;
pub mod used;
pub mod views;

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

/// Where `meta`'s package keeps its resources for `person` (the lowercase hex of their user root
/// key): their own record of the workspace when the app declares `records: .own`
/// ([`member_record_of`]), which only that person's devices read or change and every api decides
/// as theirs alone. `None` for the workspace's record of the app, which every member reads; and
/// for a person without a root identity, who is a member of no workspace, so nobody else reads
/// what they keep.
#[must_use]
pub fn own_record(meta: &tilolang::PackageMeta, person: &str) -> Option<String> {
    meta.own_records
        .then(|| member_record_of(&meta.app, person))
        .flatten()
}

/// `person`'s own record of the app `app` in the workspace, of the app's package kind
/// (`tl.<app>`), which a package declaring `records: .own` keeps each member's own; `None` for a
/// person without a root identity.
#[must_use]
pub fn member_record_of(app: &str, person: &str) -> Option<String> {
    collab_types::OwnResource::of_person(&person.to_ascii_lowercase(), &format!("tl.{app}"))
        .map(|own| own.document_id())
}

/// Whether the app `app`'s manifest declares its records each member's own (`records: .own`),
/// as the build carries it or the package's surface marks it.
#[must_use]
pub fn keeps_own_records(app: &str) -> bool {
    collab_types::own_kinds(app).contains(&format!("tl.{app}"))
}

/// Whether `person` reaches what the record `record` of the app `app` holds: in an app whose
/// manifest declares its records each member's own (`records: .own`), only the person's own
/// record ([`member_record_of`]), whatever their role in the workspace, as every api decides it;
/// in any other app, whatever the workspace policy decides. An agent reaches it as the person it
/// acts for; an invoker acting for nobody reaches no one's own record.
#[must_use]
pub fn reaches(app: &str, record: Option<&str>, person: Option<&str>) -> bool {
    if !keeps_own_records(app) {
        return true;
    }
    record.is_none_or(|record| {
        person
            .and_then(|person| member_record_of(app, person))
            .is_some_and(|own| own == record)
    })
}
