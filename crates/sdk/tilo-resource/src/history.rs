//! Version history of any resource, derived from its document's change graph:
//! who changed it, when, through which action — and what it looked like then.
//!
//! Nothing here is specific to a resource type. The attribution comes from
//! the executor (the change's actor is the author, its message carries the
//! action's [`ActionContext`]); materialising a past version is
//! [`Resource::read`] on the document forked at that version's heads.

use automerge::ChangeHash;
use tilo_actions::ActionContext;

use crate::model::{Doc, ModelError};
use crate::resource::Resource;

/// One change on a resource's timeline.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Version {
    pub hash: ChangeHash,
    /// Hex of the authoring actor.
    pub actor: String,
    /// Milliseconds since the Unix epoch, or 0 when the change carries no time.
    pub time_ms: i64,
    /// What the change did, when it was made through an action.
    pub context: Option<ActionContext>,
    /// The raw change message.
    pub message: Option<String>,
}

impl Version {
    /// A one-line human description: the action's label when known.
    pub fn summary(&self) -> Option<String> {
        self.context
            .as_ref()
            .and_then(ActionContext::summary)
            .or_else(|| self.message.clone())
    }
}

/// Every change in causal order, oldest first.
pub fn versions(doc: &mut Doc) -> Vec<Version> {
    doc.get_changes(&[])
        .into_iter()
        .map(|change| {
            let message = change.message().map(str::to_owned);
            Version {
                hash: change.hash(),
                actor: change.actor_id().to_hex_string(),
                time_ms: change.timestamp().saturating_mul(1000),
                context: message
                    .as_deref()
                    .and_then(ActionContext::from_change_message),
                message,
            }
        })
        .collect()
}

/// The resource as it was at `heads`.
///
/// # Errors
/// Returns [`ModelError`] when that version does not hold this resource.
pub fn read_at<R: Resource>(doc: &mut Doc, heads: &[ChangeHash]) -> Result<R::State, ModelError> {
    let past = doc.fork_at(heads)?;
    R::read(&past)
}
