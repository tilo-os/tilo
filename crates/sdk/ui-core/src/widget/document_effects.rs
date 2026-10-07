//! Optional outside consequences of an actually applied document action.
//!
//! This is separate from History's immutable cache-reconciliation hook. The
//! executor alone constructs the change, and custody comes from the current
//! admitted invocation, never from document fields or an earlier effect.

use super::{ResourceOperation, ResourceOperationStart};
use automerge::{AutoCommit, ChangeHash};

pub struct AppliedDocument<'a> {
    pub(crate) invocation: tilo_actions::InvocationId,
    pub(crate) before: &'a [ChangeHash],
    pub(crate) document: &'a AutoCommit,
    pub(crate) output: tilo_actions::ActionOutput,
}

impl AppliedDocument<'_> {
    /// The local operation whose asynchronous acknowledgement is pending.
    /// Custody may come from a separately retained source invocation.
    pub fn invocation(&self) -> tilo_actions::InvocationId {
        self.invocation
    }
    pub fn before(&self) -> &[ChangeHash] {
        self.before
    }
    pub fn document(&self) -> &AutoCommit {
        self.document
    }
    pub fn output(&self) -> &tilo_actions::ActionOutput {
        &self.output
    }
}

/// An owner that explicitly drains caller-authorized document consequences.
/// The owner must retain the operation's actual custody with delayed work and
/// report its terminal result through the ordinary resource operation poll.
/// Only actions declared to complete asynchronously enter this callback.
/// Immediate actions, submissions, replay and remote merge do not enter it.
pub trait DocumentEffects {
    /// # Errors
    /// Returns refusal of the current caller or the exact queued consequence.
    fn start_document_effects(
        &mut self,
        operation: &ResourceOperation<'_>,
        applied: &AppliedDocument<'_>,
    ) -> Result<Option<ResourceOperationStart>, String>;
}
