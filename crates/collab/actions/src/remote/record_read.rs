//! Record reads outside their app: how an agent task's host reads a workspace's records
//! (`docs/designs/app-language/agents.md`, "Reading records outside their app").
//!
//! A workspace's records are sealed per app: a connection holding that app's launch custody opens
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

/// What a caller that is neither a person nor an agent task acting for its person is told.
pub const RECORD_READS_FOR_TASKS: &str = "only a person, or an agent acting for the person who \
     admitted it, reads records outside their app; a person's device hands its agents nothing here";

/// One record of a workspace, by its app (the id whose records it is) and its id there (empty for
/// the app's record of the workspace).
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
    /// The workspace holds no such record: nothing was ever written to it.
    Absent,
}

/// A host's door for reading records outside their app.
pub trait RecordReads: Send + Sync {
    /// The record `address` of `caller`'s workspace, as `caller` may read it now.
    ///
    /// # Errors
    ///
    /// [`ActionHostError::NotPermitted`] for a caller that is not an agent task acting for its
    /// person, [`ActionHostError::Refused`] for a record the caller may not read,
    /// [`ActionHostError::Unavailable`] when the workspace's records cannot be read now (still
    /// arriving, or held nowhere on this device).
    fn read(&self, caller: &Caller, address: &RecordAddress)
    -> Result<RecordCopy, ActionHostError>;

    /// Wake `wake` once the next write to `address` lands on this host, decided as a read is:
    /// what a reader holding a copy waits on before it reads again. Watched before the read it
    /// guards, so no write between the two is missed.
    ///
    /// # Errors
    ///
    /// As [`Self::read`]; a host that cannot tell when a record changes answers
    /// [`ActionHostError::Unavailable`].
    fn watch(
        &self,
        caller: &Caller,
        address: &RecordAddress,
        wake: &RecordWake,
    ) -> Result<(), ActionHostError> {
        let _ = (caller, address, wake);
        Err(ActionHostError::Unavailable {
            reason: RECORD_READS_UNAVAILABLE.into(),
        })
    }
}

/// Woken once, by the first write to any record it watches ([`RecordReads::watch`]).
#[derive(Clone)]
pub struct RecordWake(std::sync::Arc<std::sync::Mutex<Option<super::Completer<()>>>>);

impl RecordWake {
    /// A wake, and what its reader waits on.
    #[must_use]
    pub fn new() -> (Self, super::Pending<()>) {
        let (woken, waiting) = super::pending();
        (
            Self(std::sync::Arc::new(std::sync::Mutex::new(Some(woken)))),
            waiting,
        )
    }

    /// A write landed: wake the reader, once.
    pub fn wake(&self) {
        if let Some(woken) = self.0.lock().ok().and_then(|mut woken| woken.take()) {
            woken.complete(());
        }
    }

    /// Whether this wake can still wake anyone: not woken yet, and its reader still waits.
    #[must_use]
    pub fn waiting(&self) -> bool {
        self.0
            .lock()
            .is_ok_and(|woken| woken.as_ref().is_some_and(|woken| !woken.is_abandoned()))
    }
}

/// An action to run on one record at the record's origin, as an agent task invokes it: the
/// record (by its app and its id there), the action's subject resource and the action, its
/// arguments, and what the task's preview planned, when it planned.
#[derive(Clone, Debug, PartialEq)]
pub struct RecordChange {
    pub app: String,
    pub record: String,
    pub resource: String,
    pub action: String,
    pub arguments: serde_json::Map<String, serde_json::Value>,
    pub preview: Option<serde_json::Value>,
}

/// A host's door for an agent task's changes to records outside their apps. A change goes to
/// the record's origin, which runs the action against the record as it is then, applies what it
/// wrote as the task's step and answers with its JSON: `{"ok": {value, resources}, "differs":
/// bool}`, or `{"failed": why}` when the action did not run.
pub trait RecordChanges: Send + Sync {
    /// Apply `change` for `caller`, as `caller` may change the record now.
    ///
    /// # Errors
    ///
    /// [`ActionHostError::Refused`] for a caller that is not an agent task acting for its
    /// person or a change the workspace does not allow it, [`ActionHostError::ConfirmationRequired`]
    /// for one that asks its person first, [`ActionHostError::Unavailable`] when the origin
    /// cannot be reached.
    fn apply(
        &self,
        caller: &Caller,
        change: &RecordChange,
    ) -> Result<serde_json::Value, ActionHostError>;
}
