//! What changed in a workspace's records, as the provider that keeps them saw
//! it: every change to every record, with who made it and in which run, for
//! a caller following the whole workspace rather than one resource.
//!
//! A record's own `history` verb lists its changes to whoever reads that
//! record. A caller watching the workspace (an agent's `tilo_watch` with no
//! path, the API's event stream) needs every record's, including records it
//! never named, so the provider that holds the records answers from what it
//! holds: [`ChangeFeed`], offered through [`super::ActionProvider::change_feed`]
//! as the roster is through [`super::ActionProvider::here`], and asked across the
//! wire with [`super::RemoteActionMessage::ChangesRequest`].

use serde::{Deserialize, Serialize};

use super::{ActionCallError, Caller, Pending};

/// What a provider without a change feed answers.
pub const CHANGE_FEED_UNAVAILABLE: &str =
    "this provider keeps no feed of its records' changes; watch one resource by its path instead";

/// What a change did to the resource it is a change of: it made it, edited it, or removed it. The
/// store reads it from the recorded change, and every watcher of the workspace is told it beside
/// the change's id, so no app works it out from what it saw before. A change always names one:
/// a change that says nothing is a fault, never an edit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum What {
    /// The resource's first recorded change, which made it.
    Created,
    /// A change to the resource itself.
    Edited,
    /// The change that removed it.
    Removed,
}

impl What {
    /// The word an event carries it as: `created`, `edited` or `removed`.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Edited => "edited",
            Self::Removed => "removed",
        }
    }

    /// The [`Self::word`] `word` names.
    #[must_use]
    pub fn of_word(word: &str) -> Option<Self> {
        match word {
            "created" => Some(Self::Created),
            "edited" => Some(Self::Edited),
            "removed" => Some(Self::Removed),
            _ => None,
        }
    }
}

/// One change to one record of the workspace.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ResourceChanged {
    /// The app whose record it is.
    pub app: String,
    /// The record's id there; empty for the app's record of the workspace.
    pub record: String,
    /// The change as the record's `history` lists it: its `change` id, its
    /// `sentence`, `time_ms`, who made it (`actor`, `by`), the group it was a
    /// step of (`title`, `group`, `group_kind`), the invocation that made
    /// it (`invocation`), and what it did to the resource (`happened`, a [`What`]).
    pub entry: serde_json::Value,
}

/// The changes after a cursor, oldest first, and the cursor after them.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ChangePage {
    pub changes: Vec<ResourceChanged>,
    /// Opaque: handed back as it is to read on from here.
    pub cursor: String,
}

/// A provider's feed of every change to the records of a caller's workspace.
pub trait ChangeFeed: Send + Sync {
    /// The changes `caller` may read made after `after`, a cursor an earlier
    /// page answered; `None` starts from now, answering no changes and the
    /// cursor to read on from. A cursor this provider cannot read on from (it
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
    ) -> Pending<Result<ChangePage, ActionCallError>>;
}
