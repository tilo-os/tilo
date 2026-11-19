//! What an app asks of the device's one store, and what it is answered, as they cross the app's
//! connection. An app holds no store of its own: it writes and reads resources by kind and id,
//! and the device answers from its store, as the person the app acts for may read.

use serde::{Deserialize, Serialize};

use crate::admit::Stamp;
use crate::index::Indexed;
use crate::query::{Filter, Page, Query};
use crate::store::Resource;

/// The most a command or its answer takes on the wire.
pub const MAX_WIRE_BYTES: usize = 4 << 20;

/// One thing an app asks of the store.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Command {
    /// Write the resource `id` of `kind`, as the action `stamp` names.
    Put {
        kind: String,
        id: String,
        resource: Resource,
        stamp: Stamp,
    },
    /// Remove the resource `id` of `kind`, as `stamp` names.
    Remove {
        kind: String,
        id: String,
        stamp: Stamp,
    },
    /// Take back the invocation `invocation`, as `stamp` names.
    Undo { invocation: String, stamp: Stamp },
    /// The resource `id` of `kind`.
    Get { kind: String, id: String },
    /// One page of `query`.
    Query(Query),
    /// How many resources of `kind` hold to `filters`.
    Count { kind: String, filters: Vec<Filter> },
}

/// What the store answered.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Reply {
    /// The write was made.
    Written,
    /// The resource, `None` where there is none the person may read.
    Resource(Option<Indexed>),
    Page(Page),
    Count(usize),
}
