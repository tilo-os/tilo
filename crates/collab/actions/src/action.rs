//! The [`Action`] trait and its serializable envelope-side vocabulary.
//!
//! An **Action** is the atomic unit of the "play locally, replay anywhere"
//! model: a *value* that deterministically mutates a CRDT document. The same
//! action, applied to two replicas that start from the same state, drives both
//! to the *identical* document — because the [executor](crate::executor) pins
//! the Automerge actor id and commit timestamp from the action's envelope, the
//! resulting change is content-addressed identical on every replica (so if the
//! replicas ever exchange native Automerge changes, the duplicate dedupes
//! instead of double-applying).
//!
//! Actions are **not** presentation. Rendering, selection, scroll — anything
//! that is local-only and never leaves the machine — must never go through an
//! action. Only mutations to shared CRDT state do.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// The CRDT document an action mutates. Aliased so call sites read as
/// "mutate the Doc" rather than leaking the concrete Automerge type
/// everywhere; it is exactly [`automerge::AutoCommit`].
pub type Doc = automerge::AutoCommit;

/// Where an action *prefers* to execute.
///
/// This is a declared property of an action **type**, carried on every
/// envelope so the transport can honor it. Only [`ClientLocal`](Self::ClientLocal)
/// is fully realized today; [`ServerPreferred`](Self::ServerPreferred) is a real,
/// transport-honored preference (see [`crate::executor::dispatch`]) that falls
/// back to local execution when no host is reachable — not a `TODO`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ExecutionLocus {
    /// This client applies the action to its local replica and replicates the
    /// envelope; peers replay it (and, by replaying, verify it was a valid
    /// mutation of the shared state). This is the default.
    #[default]
    ClientLocal,
    /// The action prefers authoritative execution on a host/server (e.g. a
    /// shared queue or a moderated reply the host must serialize). The
    /// dispatcher submits the envelope to the host; if no host is reachable it
    /// falls back to `ClientLocal` semantics so the user is never blocked.
    ServerPreferred,
}

/// A globally-unique id minted for each dispatched action. Its 16 bytes double
/// as the deterministic Automerge actor id used to commit the action's change,
/// which is what makes replay content-addressed identical across replicas.
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
    /// The action decoded and was well-formed, and the app declined to carry
    /// it out — in the app's own words, for a person to read.
    ///
    /// Distinct from every variant above, which are all faults in the
    /// plumbing. This one is the app answering: the content an action names is
    /// not on this machine, the thing it acts on has gone. A caller must be
    /// able to tell "I asked wrongly" from "I asked correctly and the answer
    /// is no", because only the second is worth repeating to whoever asked.
    #[error("{0}")]
    Refused(String),
}

/// A value that deterministically mutates a CRDT [`Doc`].
///
/// # Contract
///
/// * [`apply`](Action::apply) MUST be a pure function of `(self, doc-state)`:
///   no wall-clock reads, no RNG, no local-only state. Two replicas at the same
///   heads applying the same action must produce the same ops in the same order.
/// * [`apply`](Action::apply) MUST NOT commit. The [executor](crate::executor)
///   owns the commit boundary (actor id + timestamp) so the change is
///   content-addressed identical on every replica.
///
/// Concrete actions are ordinary app-defined enums that also derive
/// `Serialize` / `Deserialize`; they are registered by `type_id` in an
/// [`ActionRegistry`](crate::registry::ActionRegistry) so a replaying peer can
/// reconstruct them from the wire.
pub trait Action: std::fmt::Debug {
    /// Apply this action's mutation to `doc`. See the trait contract.
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError>;

    /// Where this action prefers to run. Defaults to [`ExecutionLocus::ClientLocal`].
    fn locus(&self) -> ExecutionLocus {
        ExecutionLocus::ClientLocal
    }
}

/// A serialized action, ready to cross a process or network boundary: the
/// registry `type_id` that names its concrete type plus the JSON payload the
/// matching decoder reconstructs it from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EncodedAction {
    pub type_id: String,
    pub payload: Vec<u8>,
}

impl EncodedAction {
    /// Serialize a concrete action under a registry `type_id`.
    pub fn encode<A>(type_id: impl Into<String>, action: &A) -> Result<Self, ActionError>
    where
        A: Action + Serialize,
    {
        Ok(Self {
            type_id: type_id.into(),
            payload: serde_json::to_vec(action).map_err(ActionError::Encode)?,
        })
    }
}
