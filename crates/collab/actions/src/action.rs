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

    /// The consequence class of this particular value — what it would do,
    /// not what its type can do. A generic edit action classifies each value
    /// (deleting a clip is [`Destructive`](crate::RiskLevel::Destructive),
    /// trimming one is not), and the host asks for the same confirmation a
    /// declared destructive action gets whenever
    /// [`requires_confirmation`](crate::RiskLevel::requires_confirmation)
    /// holds. Must be a pure function of `self`. Defaults to
    /// [`Low`](crate::RiskLevel::Low).
    fn risk(&self) -> crate::contract::RiskLevel {
        crate::contract::RiskLevel::Low
    }

    /// The document action that undoes this one when `apply` records an
    /// outside consequence in the document (an outbox entry, a filesystem op
    /// queue). Computed against the document *before* this action runs, so a
    /// delete can still read the row it removes.
    ///
    /// When this returns `Some`, undo applies that action through
    /// [`Action::apply`] instead of writing Automerge inverse patches — so the
    /// compensating queue entry is recorded the same way peers will replay it.
    /// Pure CRDT actions with no outside consequence leave this as `None`;
    /// inverse patches remain correct for them. Effects never participate:
    /// they have no heads and are not undoable.
    fn inverse_before(&self, doc: &Doc) -> Option<Self>
    where
        Self: Sized,
    {
        let _ = doc;
        None
    }

    /// [`inverse_before`](Self::inverse_before) as an encoded action, so the
    /// door can undo through `apply` after the concrete type has been erased.
    ///
    /// The palette, an agent and a script apply a decoded `dyn Action`. That
    /// value cannot return `Self`. An action whose `apply` writes an outside
    /// queue implements this and encodes the compensating action under the
    /// same registry type id. The default is `None`: undo stays Automerge
    /// inverse patches.
    fn inverse_encoded(&self, doc: &Doc) -> Option<EncodedAction> {
        let _ = doc;
        None
    }

    /// Whether this value changes the document's content or is housekeeping
    /// about it (a read marker, a reading position). The executor writes it
    /// into the change's [`ActionContext`](crate::ActionContext), where a
    /// workspace activity feed reads it. Must be a pure function of `self`.
    /// Defaults to [`Content`](crate::context::ChangeKind::Content).
    fn change_kind(&self) -> crate::context::ChangeKind {
        crate::context::ChangeKind::Content
    }

    /// What kind of action this value is: whether it only changes the
    /// document or also reaches outside it. Must be a pure function of
    /// `self`. An action that records an outside consequence in the document
    /// (an outbox entry) is [`Outside`](crate::ActionKind::Outside); the
    /// default is [`Undoable`](crate::ActionKind::Undoable).
    fn action_kind(&self) -> crate::ActionKind {
        crate::ActionKind::Undoable
    }

    /// Whether this value records a message someone received (a chat post,
    /// a delivered mail). The executor marks its change with
    /// [`INCOMING_MESSAGE_FACET`](crate::INCOMING_MESSAGE_FACET). Must be a
    /// pure function of `self`.
    fn incoming_message(&self) -> bool {
        false
    }

    /// Whether this value loses something Undo cannot bring back, beside
    /// the document change Undo reverts (an account's secret every device
    /// forgets). See [`IRREVERSIBLE_TAG`](crate::IRREVERSIBLE_TAG). Must be
    /// a pure function of `self`.
    fn irreversible(&self) -> bool {
        false
    }

    /// What this value does and to which item, as history records it: a
    /// field edit with its values before and after, a verb with its plain
    /// arguments, or a group of those. Read from `doc` as it is before the
    /// value applies, on the origin only; replicas replay it with the
    /// change. `None` lets the executor describe the value as the verb its
    /// encoded form names ([`crate::Described::from_encoded`]).
    fn describe(&self, doc: &Doc) -> Option<crate::Described> {
        let _ = doc;
        None
    }
}

/// Whether Undo takes back a value with these facts: it changes the document
/// alone ([`ActionKind::Undoable`](crate::ActionKind::Undoable)), as a
/// person's step ([`ChangeKind::is_step`](crate::ChangeKind::is_step)), and
/// loses nothing Undo cannot bring back ([`Action::irreversible`]). The one
/// rule every host asks, whatever the value's type.
#[must_use]
pub fn undo_takes_back(
    kind: crate::ActionKind,
    change: crate::ChangeKind,
    irreversible: bool,
) -> bool {
    kind == crate::ActionKind::Undoable && change.is_step() && !irreversible
}

/// Whether a value with these facts is confirmed before it applies: its risk
/// asks for confirmation and Undo cannot take it back
/// ([`RiskLevel::requires_confirmation_when`](crate::RiskLevel::requires_confirmation_when)).
#[must_use]
pub fn asks_first(
    risk: crate::RiskLevel,
    kind: crate::ActionKind,
    change: crate::ChangeKind,
    irreversible: bool,
) -> bool {
    risk.requires_confirmation_when(undo_takes_back(kind, change, irreversible))
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
        Self::encoded(type_id.into(), serde_json::to_vec(action))
    }

    /// The tail of [`Self::encode`] every action type shares.
    #[inline(never)]
    fn encoded(
        type_id: String,
        payload: Result<Vec<u8>, serde_json::Error>,
    ) -> Result<Self, ActionError> {
        Ok(Self::written(
            type_id,
            payload.map_err(ActionError::Encode)?,
        ))
    }

    /// An action of `type_id` whose payload is `payload`, a group's written
    /// with what its members share once ([`crate::group_form`]).
    #[must_use]
    pub fn written(type_id: String, payload: Vec<u8>) -> Self {
        let payload = if crate::group_form::is_group(&type_id) {
            crate::group_form::compact(&payload).unwrap_or(payload)
        } else {
            payload
        };
        Self { type_id, payload }
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

#[cfg(test)]
mod operation_kind_tests {
    use super::OperationKind;

    /// Only the canonical form round-trips; other spellings are refused.
    #[test]
    fn operation_kind_accepts_only_the_canonical_spelling() {
        for (kind, canonical, lower) in [
            (OperationKind::Document, "Document", "document"),
            (OperationKind::Effect, "Effect", "effect"),
            (OperationKind::View, "View", "view"),
        ] {
            assert_eq!(serde_json::to_value(kind).unwrap(), canonical);
            assert_eq!(
                serde_json::from_value::<OperationKind>(serde_json::json!(canonical)).unwrap(),
                kind
            );
            assert!(serde_json::from_value::<OperationKind>(serde_json::json!(lower)).is_err());
        }
        assert!(serde_json::from_value::<OperationKind>(serde_json::json!("EFFECT")).is_err());
    }
}
