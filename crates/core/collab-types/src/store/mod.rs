//! What a process asks of the workspace's resource store and what it is answered: resources as a
//! write gives them and as the index holds them, queries of a kind, and the commands and replies
//! that carry them ([`wire`]). The store itself (`tilo_resource_store`) places resources, keeps
//! the index and carries the commands out.

mod query;
mod resource;
mod value;
pub mod wire;

pub use query::{Absent, Cursor, Filter, Order, Page, Query, Sort, past};
pub use resource::{Indexed, PARENT, PastChild, Resource, Review};
pub use value::{Cell, Value};

/// The kind of the workspace's members' resource: who is in the workspace (`people`), as every
/// store reading the workspace answers it. A store a workspace keeps answers it from the
/// workspace's governance; one held in a process for a device of its own keeps it as a record.
pub const WORKSPACE_MEMBERS_KIND: &str = "workspace.members";
/// The id the workspace's members' resource is read by ([`WORKSPACE_MEMBERS_KIND`]).
pub const WORKSPACE_MEMBERS_ID: &str = "workspace/members";
