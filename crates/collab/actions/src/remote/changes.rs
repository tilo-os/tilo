//! What changed in a workspace's records, as the host that keeps them saw
//! it: every change to every record, with who made it and in which run, for
//! a caller following the whole workspace rather than one resource.
//!
//! A record's own `history` verb lists its changes to whoever reads that
//! record. A caller watching the workspace (an agent's `tilo_watch` with no
//! path, the API's event stream) needs every record's, including records it
//! never named, so the host that holds the records answers from what it
//! holds: [`ChangeFeed`], offered through [`super::ActionHost::change_feed`]
//! as the roster is through [`super::ActionHost::here`], and asked across the
//! wire with [`super::RemoteActionMessage::ChangesRequest`].

use serde::{Deserialize, Serialize};

use super::{ActionHostError, Caller, Pending};

/// What a host without a change feed answers.
pub const CHANGE_FEED_UNAVAILABLE: &str =
    "this host keeps no feed of its records' changes; watch one resource by its path instead";

/// One change to one record of the workspace.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RecordChanged {
    /// The app whose record it is.
    pub app: String,
    /// The record's id there; empty for the app's record of the workspace.
    pub record: String,
    /// The change as the record's `history` lists it: its `change` id, its
    /// `sentence`, `time_ms`, who made it (`actor`, `by`), the run it was a
    /// step of (`run`, `run_id`) and the invocation that made it
    /// (`invocation`).
    pub entry: serde_json::Value,
}

/// The changes after a cursor, oldest first, and the cursor after them.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ChangePage {
    pub changes: Vec<RecordChanged>,
    /// Opaque: handed back as it is to read on from here.
    pub cursor: String,
}

/// A host's feed of every change to the records of a caller's workspace.
pub trait ChangeFeed: Send + Sync {
    /// The changes `caller` may read made after `after`, a cursor an earlier
    /// page answered; `None` starts from now, answering no changes and the
    /// cursor to read on from. A cursor this host cannot read on from (it
    /// restarted, or the changes it named are no longer kept) answers from
    /// now too, so a follower misses changes rather than reading old ones
    /// twice.
    ///
    /// # Errors
    /// The feed refuses `caller`, or the workspace cannot be read now.
    fn since(
        &self,
        caller: &Caller,
        after: Option<&str>,
    ) -> Pending<Result<ChangePage, ActionHostError>>;
}
