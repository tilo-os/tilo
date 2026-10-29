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
pub mod catalogue;
pub mod effects;
pub mod host;
pub mod index;
pub mod instance;
pub mod items;
pub mod members;
pub mod meta;
pub mod names;
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
pub mod task;
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

/// The record kind of an app's record in a workspace: every tilolang package keeps its resources
/// in one.
pub const RECORD_KIND: &str = "tilolang.room";

/// Where `meta`'s package keeps its resources for `person` (the lowercase hex of their user root
/// key): their own record of the workspace when the app declares `records: .own`
/// (`tilo_action_policy::member_record`), which only that person's devices read or change and
/// every door decides as theirs alone. `None` for the workspace's record of the app, which
/// every member reads; and for a person without a root identity, who is a member of no
/// workspace, so nobody else reads what they keep.
#[must_use]
pub fn own_record(meta: &tilolang::PackageMeta, person: &str) -> Option<String> {
    meta.own_records.then(|| member_record_of(person)).flatten()
}

/// `person`'s own record of the workspace, the one an app with `records: .own` keeps theirs in;
/// `None` for a person without a root identity.
#[must_use]
pub fn member_record_of(person: &str) -> Option<String> {
    let id = tilo_action_policy::member_record_id(&person.to_ascii_lowercase(), RECORD_KIND);
    tilo_action_policy::member_record(&id)
        .is_some()
        .then_some(id)
}

/// Whether `person` reaches what the record `record` holds: a member's own record
/// ([`member_record_of`]) is that person's alone, whatever their role in the room, as every door
/// decides it; any other record is the room policy's to decide. An agent reaches it as the
/// person it acts for; an invoker acting for nobody reaches no one's own record.
#[must_use]
pub fn reaches(record: Option<&str>, person: Option<&str>) -> bool {
    match record.and_then(tilo_action_policy::member_record) {
        Some((owner, _)) => person.is_some_and(|person| person.eq_ignore_ascii_case(owner)),
        None => true,
    }
}
