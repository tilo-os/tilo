//! Optional outside consequences of an actually applied document action.
//!
//! This is separate from History's immutable cache-reconciliation hook. The executor alone
//! constructs the change, and ownership comes from the current admitted invocation, never
//! from document fields or an earlier effect.

use super::{ResourceOperation, ResourceOperationStart};
use automerge::{AutoCommit, ChangeHash};

pub struct AppliedDocument<'a> {
    pub invocation: tilo_actions::InvocationId,
    pub before: &'a [ChangeHash],
    pub document: &'a AutoCommit,
    pub output: tilo_actions::ActionOutput,
}

impl AppliedDocument<'_> {
    /// The local operation whose asynchronous acknowledgement is pending.
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
pub trait DocumentEffects {
    fn start_document_effects(
        &mut self,
        operation: &ResourceOperation<'_>,
        applied: &AppliedDocument<'_>,
    ) -> Result<Option<ResourceOperationStart>, String>;
}
