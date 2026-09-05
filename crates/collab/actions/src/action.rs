//! The [`Action`] trait and its serializable envelope-side vocabulary.
//!
//! An **Action** is the atomic unit of "play locally, replay anywhere":
//! a *value* that deterministically mutates a CRDT document. The
//! [executor](crate::executor) pins the Automerge actor id and commit
//! timestamp from the action's envelope, so the resulting change is
//! content-addressed identical on every replica (native Automerge
//! exchange later dedupes rather than double-applying).
//!
//! Actions are **not** presentation — local-only state (rendering,
//! selection, scroll) must never go through an action; only shared CRDT
//! mutations do.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// The CRDT document an action mutates — aliased so call sites read as
/// "mutate the Doc"; exactly [`automerge::AutoCommit`].
pub type Doc = automerge::AutoCommit;

/// Where an action executes, declared on the action type and carried on every
/// envelope. [`ServerPreferred`](Self::ServerPreferred) retains its historical
/// wire spelling, but is an authoritative-host requirement: absence of a host
/// is a refusal, never permission for the client to perform the operation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ExecutionLocus {
    /// This client applies the action locally and replicates the envelope;
    /// peers replay it (verifying it was a valid mutation). The default.
    #[default]
    ClientLocal,
    /// Requires authoritative execution on a host (e.g. a shared queue or
    /// moderated reply). The dispatcher submits to the host and fails closed
    /// when none is reachable.
    ServerPreferred,
}

/// What an exported operation changes. Keeping this explicit prevents
/// applications from disguising local effects or navigation as CRDT edits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OperationKind {
    /// Deterministic shared-document mutation, decoded by `ActionRegistry`.
    #[default]
    Document,
    /// Local or host-owned side effect (launch, filesystem, network, media).
    Effect,
    /// Local presentation/navigation change (selection, route, focused view).
    View,
}

impl OperationKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Document => "document",
            Self::Effect => "effect",
            Self::View => "view",
        }
    }
}

/// A globally-unique id minted per dispatched action. Its 16 bytes
/// double as the deterministic Automerge actor id, making replay
/// content-addressed identical across replicas.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ActionId(pub Uuid);

impl ActionId {
    /// Mint a fresh random id.
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    /// The 16 raw bytes, used verbatim as the Automerge actor id.
    pub fn actor_bytes(&self) -> [u8; 16] {
        *self.0.as_bytes()
    }
}

impl Default for ActionId {
    fn default() -> Self {
        Self::new()
    }
}

/// Errors from encoding, decoding, or applying an action.
#[derive(Debug, thiserror::Error)]
pub enum ActionError {
    /// The action's `apply` hit an Automerge error.
    #[error("automerge error: {0}")]
    Automerge(#[from] automerge::AutomergeError),
    /// No decoder is registered for this action's `type_id`.
    #[error("no decoder registered for action type `{0}`")]
    UnknownType(String),
    /// The payload could not be deserialized into the registered type.
    #[error("failed to decode action `{type_id}`: {source}")]
    Decode {
        type_id: String,
        source: serde_json::Error,
    },
    /// The concrete action could not be serialized into a payload.
    #[error("failed to encode action: {0}")]
    Encode(serde_json::Error),
    /// The action was well-formed and the app declined to carry it out, in
    /// its own words. Every other variant is a plumbing fault; this one is
    /// the app answering — a caller must tell "I asked wrongly" from "I
    /// asked correctly and the answer is no".
    #[error("{0}")]
    Refused(String),
}

/// A value that deterministically mutates a CRDT [`Doc`].
///
/// [`apply`](Action::apply) MUST be a pure function of `(self,
/// doc-state)` — no wall-clock reads, no RNG, no local-only state — and
/// MUST NOT commit; the [executor](crate::executor) owns the commit
/// boundary (actor id + timestamp) so the change is content-addressed
/// identical everywhere.
///
/// Concrete actions are app-defined enums deriving `Serialize`/
/// `Deserialize`, registered by `type_id` in an
/// [`ActionRegistry`](crate::registry::ActionRegistry) so a replaying
/// peer can reconstruct them.
pub trait Action: std::fmt::Debug {
    /// Apply this action's mutation to `doc`. See the trait contract.
    ///
    /// # Errors
    ///
    /// Implementations return [`ActionError::Automerge`] for a document
    /// write failure, or [`ActionError::Refused`] if the app declines to
    /// carry out an otherwise well-formed action.
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError>;

    /// Where this action prefers to run. Defaults to [`ExecutionLocus::ClientLocal`].
    fn locus(&self) -> ExecutionLocus {
        ExecutionLocus::ClientLocal
    }
}

/// A serialized action ready to cross a process/network boundary: the
/// registry `type_id` plus the JSON payload the matching decoder
/// reconstructs it from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EncodedAction {
    pub type_id: String,
    pub payload: Vec<u8>,
}

impl EncodedAction {
    /// Serialize a concrete action under a registry `type_id`.
    ///
    /// # Errors
    ///
    /// Returns [`ActionError::Encode`] if `action` fails to serialize.
    pub fn encode<A>(type_id: impl Into<String>, action: &A) -> Result<Self, ActionError>
    where
        A: Action + Serialize,
    {
        Ok(Self {
            type_id: type_id.into(),
            payload: serde_json::to_vec(action).map_err(ActionError::Encode)?,
        })
    }

    /// Serialize a non-document operation payload. Unlike [`Self::encode`],
    /// this intentionally requires no [`Action`] implementation: effects and
    /// views are handled by the owning application, not replayed into a CRDT.
    ///
    /// # Errors
    ///
    /// Returns [`ActionError::Encode`] if `payload` fails to serialize.
    pub fn encode_operation<T>(type_id: impl Into<String>, payload: &T) -> Result<Self, ActionError>
    where
        T: Serialize,
    {
        Ok(Self {
            type_id: type_id.into(),
            payload: serde_json::to_vec(payload).map_err(ActionError::Encode)?,
        })
    }
}
