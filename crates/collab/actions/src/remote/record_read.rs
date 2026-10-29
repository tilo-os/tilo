//! Record reads outside their app: how an agent task's host reads a room's records
//! (`docs/designs/app-language/agents.md`, "Reading records outside their app").
//!
//! A room's records are sealed per app: a connection holding that app's launch custody opens
//! them. An agent task is not an app. Its host is admitted as the agent acting for the person
//! who started it, never session-delegated and holding no app's route, and reads one record at
//! a time through this door. Each read is decided by the check every reader of a record goes
//! through (the lesser of the agent's grants and the person's), again for every read, so a
//! grant revoked or a member removed in the middle of a task cuts the reads that follow. A read
//! carries the record's committed content and nothing that writes to it: the task changes a
//! record only by invoking actions.

use super::{ActionHostError, Caller};

/// What a host without record reads answers.
pub const RECORD_READS_UNAVAILABLE: &str = "this host reads no records outside their apps";

/// What a caller that is not an agent task acting for its person is told.
pub const RECORD_READS_FOR_TASKS: &str = "only an agent acting for the person who admitted it \
     reads records outside their app; a person's device hands its agents nothing here";

/// One record of a room, by its app (the id whose records it is) and its id there (empty for
/// the app's record of the room).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct RecordAddress {
    pub app: String,
    pub record: String,
}

/// What a read of one record answers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecordCopy {
    /// The record's committed content: an Automerge document, whole.
    Document(Vec<u8>),
    /// The room holds no such record: nothing was ever written to it.
    Absent,
}

/// A host's door for reading records outside their app.
pub trait RecordReads: Send + Sync {
    /// The record `address` of `caller`'s room, as `caller` may read it now.
    ///
    /// # Errors
    ///
    /// [`ActionHostError::NotPermitted`] for a caller that is not an agent task acting for its
    /// person, [`ActionHostError::Refused`] for a record the caller may not read,
    /// [`ActionHostError::Unavailable`] when the room's records cannot be read now (still
    /// arriving, or held nowhere on this device).
    fn read(&self, caller: &Caller, address: &RecordAddress)
    -> Result<RecordCopy, ActionHostError>;
}
