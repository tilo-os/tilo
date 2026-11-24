//! The resource store: every resource the store keeps, indexed by its kind and queried the same
//! way whichever language declared it.
//!
//! An app declares its resource types ([`Schema`]): the fields of each and which types nest in
//! which, as the surface it publishes from its code says, whichever language it is written in.
//! It never says how a resource is stored: the store alone decides where each resource lives,
//! on which devices and in which document, and it is the store that tells the index what a
//! commit changed.
//!
//! The index is updated in the same apply that commits a change ([`CommitHook`]): whoever
//! applies a change to a workspace document hands the index the resources it changed
//! ([`Commit`]), so a reader in the committing process sees its own write at once, and a query
//! never waits on a catch-up. A resource is indexed under its kind wherever the store placed it,
//! so the resources of one kind read as one list across documents, and those nested in one
//! resource as the list a filter on [`PARENT`] names.
//!
//! What is indexed is what a filter, a sort or a count reads: the scalar fields and sets of
//! scalars a type declares. Rich text, lists, maps and other bulk content are the resource's
//! own and never copied here; a read of them goes to the document the resource names.
//!
//! Queries ([`Query`]) page through a kind in its order or by a field, filtered by values,
//! ranges, set members and text, and resume from a [`Cursor`]. A write costs the logarithm of
//! the kind's size per field it indexes, and a page costs what it shows plus what its filters
//! skip, so both stay flat as a kind grows.

pub mod admit;
mod history;
mod index;
mod port;
mod query;
mod ranked;
mod schema;
mod snapshot;
pub mod store;
mod undo;
mod value;
pub mod wire;

pub use admit::{
    Admit, Group, Origin, Proposal, Received, Refusal, Stamp, Touch, Touched, touches,
};
pub use history::{Nested, Version, VersionKind, touched_by};
pub use index::{Change, Commit, CommitHook, Index, Indexed, PARENT};
pub use port::{LocalPort, Port};
pub use query::{Absent, Cursor, Filter, Order, Page, Query, Sort};
pub use schema::{Expected, FIELD_EDIT, Field, FieldKind, Schema, Type, Verb, Writes};
pub use store::{
    CHANGED, CREATED, Live, RecordReader, Resource, Source, Store, StoreError, UPDATED, Written,
    Wrote,
};
pub use value::{Cell, Value};
