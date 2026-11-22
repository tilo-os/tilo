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
pub mod asks;
pub mod authority;
pub mod background;
pub mod bundle;
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

/// Where `meta`'s package keeps its resources for `person` (the lowercase hex of their user root
/// key): their own document of the workspace when the app declares `resources: .own`
/// ([`member_document_of`]), which only that person's devices read or change and every api decides
/// as theirs alone. `None` for the workspace's document of the app, which every member reads; and
/// for a person without a root identity, who is a member of no workspace, so nobody else reads
/// what they keep.
#[must_use]
pub fn own_document(meta: &tilolang::PackageMeta, person: &str) -> Option<String> {
    meta.own_resources
        .then(|| member_document_of(&meta.app, person))
        .flatten()
}

/// `person`'s own document of the app `app` in the workspace, of the app's package kind
/// (`tl.<app>`), which a package declaring `resources: .own` keeps each member's own; `None` for a
/// person without a root identity.
#[must_use]
pub fn member_document_of(app: &str, person: &str) -> Option<String> {
    collab_types::OwnResource::of_person(&person.to_ascii_lowercase(), &format!("tl.{app}"))
        .map(|own| own.document_id())
}

/// Whether the app `app`'s manifest declares its documents each member's own (`resources: .own`),
/// as the build carries it or the package's surface marks it.
#[must_use]
pub fn keeps_own_resources(app: &str) -> bool {
    collab_types::own_kinds(app).contains(&format!("tl.{app}"))
}

/// Whether `person` reaches what the document `document` of the app `app` holds: in an app whose
/// manifest declares its documents each member's own (`resources: .own`), only the person's own
/// document ([`member_document_of`]), whatever their role in the workspace, as every api decides it;
/// in any other app, whatever the workspace policy decides. An agent reaches it as the person it
/// acts for; an invoker acting for nobody reaches no one's own document.
#[must_use]
pub fn reaches(app: &str, record: Option<&str>, person: Option<&str>) -> bool {
    if !keeps_own_resources(app) {
        return true;
    }
    record.is_none_or(|record| {
        person
            .and_then(|person| member_document_of(app, person))
            .is_some_and(|own| own == record)
    })
}
