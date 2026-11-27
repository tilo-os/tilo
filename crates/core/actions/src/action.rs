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

    /// Whether a window's report of this operation may stand as a change of the person's history:
    /// a document or view change, or a read the input made (a press that asks and changes
    /// nothing). An effect changes nothing a history records, and the broker admits no report of
    /// one, so a window never reports an effect.
    #[must_use]
    pub fn reports_a_change(self, need: crate::ActionKind) -> bool {
        matches!(self, Self::Document | Self::View) || need == crate::ActionKind::Read
    }
}

pub use tilo_action_types::ActionId;

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
    /// The app declined for a reason a caller acts on rather than reads: the
    /// thing it would add is there already, say. `code` names it stably, and
    /// `message` says it in words for whoever reads.
    #[error("{message}")]
    Unavailable { code: String, message: String },
    /// The app declined for one of the reasons a window tells the person in
    /// their own language ([`Declined`]): a caller can say what happened
    /// instead of showing them the plumbing.
    #[error("{0}")]
    Declined(Declined),
}

/// The most bytes one text may hold: what one action carries, and what one
/// window carries in one string. One number for both, so a text an edit keeps
/// is always a text the window can show.
pub const MAX_TEXT_BYTES: usize = 8 * 1024 * 1024;

/// The most a conversation's running summary holds, in bytes: what an agent
/// keeps of the conversation it answers in, told to later turns in place of
/// the messages before it, and the bound a resource keeping one applies.
pub const RECALL_SUMMARY_BYTES: usize = 4 * 1024;

/// Codes an app answers a condition with, for a caller that decides on the
/// condition rather than reads the words beside it.
pub mod codes {
    /// A note imported from a source page is in the notebook already.
    pub const NOTE_ALREADY_IMPORTED: &str = "note_already_imported";
}

/// Why an app declined an action, for the reasons a person is told rather than
/// logged: the window says what happened and what to do, from the toolkit's
/// catalogue.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Declined {
    /// What the action would change is not as it read it: a newer edit reached
    /// the text first.
    Changed,
    /// The text the action carries is longer than one text may hold
    /// ([`MAX_TEXT_BYTES`]).
    TooLong,
}

impl Declined {
    /// The catalogue message that says it, and the message's arguments: the toolkit's catalogue
    /// holds the words, in every language it ships, so every window tells the same reason.
    #[must_use]
    pub fn words(self) -> (String, Vec<(String, String)>) {
        match self {
            Self::Changed => ("rich-text-changed-while-typing".to_owned(), Vec::new()),
            Self::TooLong => (
                "rich-text-too-long".to_owned(),
                // A limit of a mebibyte and more reads as its megabytes.
                vec![(
                    "limit".to_owned(),
                    format!("{} MB", MAX_TEXT_BYTES / (1024 * 1024)),
                )],
            ),
        }
    }
}

impl std::fmt::Display for Declined {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Changed => "the text changed since it was read",
            Self::TooLong => "the text is longer than one text may hold",
        })
    }
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
/// One step of an action that stands for several ([`Action::steps`]): the action, and its payload
/// under the type of the action it is a step of.
pub type ActionStep = (Box<dyn Action>, Vec<u8>);

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
    /// API can undo through `apply` after the concrete type has been erased.
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

    /// The members who alone read what this value changes, when what it
    /// changes says who reads it (a direct conversation); `None` when every
    /// member who reads the record reads it. The executor marks its change
    /// with [`READERS_FACET`](crate::READERS_FACET). Must be a pure function
    /// of `self`.
    fn readers(&self) -> Option<Vec<crate::MemberId>> {
        None
    }

    /// Whether this value leaves the system for good (a message sent to
    /// people outside it), so a person's own press of it always confirms
    /// first. See [`LEAVES_TAG`](crate::LEAVES_TAG). Must be a pure function
    /// of `self`.
    fn leaves(&self) -> bool {
        false
    }

    /// Whether this value loses something Undo cannot bring back, beside
    /// the document change Undo reverts (an account's secret every device
    /// forgets). See [`IRREVERSIBLE_TAG`](crate::IRREVERSIBLE_TAG). Must be
    /// a pure function of `self`.
    fn irreversible(&self) -> bool {
        false
    }

    /// Whether this value's undo is its own code's to compute
    /// ([`inverse_encoded`](Self::inverse_encoded) may answer one): a device
    /// applying it from an app's model module, which does not compute undo,
    /// writes no entry another device would check that undo against. Must be
    /// a pure function of `self`.
    fn compensates(&self) -> bool {
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

    /// The item this value made, read from `doc` with its writes as an open
    /// transaction: what history names a change that creates something by
    /// ("Created ‘Spring plans’"), which `doc` before the value applied could
    /// not name because the item did not exist yet. `None` when the value
    /// made no item of its own, or names none itself; the executor then keeps
    /// the description it read before the value applied.
    fn made(&self, doc: &Doc) -> Option<crate::entry::ActionItem> {
        let _ = doc;
        None
    }

    /// The actions this value is, one for each step it takes, when it stands for several: an
    /// undo of three steps is three undos of one, newest first. The executor applies each as an
    /// action of its own, so every change names the one step it moved, and a peer replays it as
    /// that one action. Each is its payload under this value's type, with the action it decodes
    /// to. `None` for a value that is one step, as almost every value is. Read from `doc` before
    /// anything applies.
    fn steps(&self, doc: &Doc) -> Option<Vec<ActionStep>> {
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

/// What a person's own press of `action` waits for, as its contract says it
/// ([`ActionContract::confirms`](crate::ActionContract::confirms)), read from
/// the value itself.
#[must_use]
pub fn press_confirms<A: Action + ?Sized>(action: &A) -> crate::Confirms {
    if action.leaves() {
        return crate::Confirms::Always;
    }
    let kind = action.action_kind();
    let kind = if undo_takes_back(kind, action.change_kind(), action.irreversible()) {
        crate::ActionKind::Undoable
    } else if kind == crate::ActionKind::Read {
        crate::ActionKind::Read
    } else {
        crate::ActionKind::Outside
    };
    kind.confirms(action.risk() == crate::RiskLevel::Destructive)
}

/// A serialized action ready to cross a process/network boundary: the
/// registry `type_id` plus the JSON payload the matching decoder
/// reconstructs it from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EncodedAction {
    pub type_id: String,
    /// Written as its text where the action is written as text (JSON), and
    /// as bytes elsewhere: never a list of numbers, one per byte.
    #[serde(with = "payload_text")]
    pub payload: Vec<u8>,
}

/// An action's payload, JSON text, as a string in a format written as text
/// and as bytes in one that is not.
mod payload_text {
    use serde::de::Visitor;
    use serde::ser::Error as _;
    use serde::{Deserializer, Serializer};

    pub(super) fn serialize<S: Serializer>(payload: &[u8], into: S) -> Result<S::Ok, S::Error> {
        if into.is_human_readable() {
            into.serialize_str(std::str::from_utf8(payload).map_err(S::Error::custom)?)
        } else {
            into.serialize_bytes(payload)
        }
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(from: D) -> Result<Vec<u8>, D::Error> {
        struct Payload;

        impl Visitor<'_> for Payload {
            type Value = Vec<u8>;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("an action's payload, as text or bytes")
            }

            fn visit_str<E: serde::de::Error>(self, text: &str) -> Result<Vec<u8>, E> {
                Ok(text.as_bytes().to_vec())
            }

            fn visit_string<E: serde::de::Error>(self, text: String) -> Result<Vec<u8>, E> {
                Ok(text.into_bytes())
            }

            fn visit_bytes<E: serde::de::Error>(self, bytes: &[u8]) -> Result<Vec<u8>, E> {
                Ok(bytes.to_vec())
            }

            fn visit_byte_buf<E: serde::de::Error>(self, bytes: Vec<u8>) -> Result<Vec<u8>, E> {
                Ok(bytes)
            }
        }

        if from.is_human_readable() {
            from.deserialize_string(Payload)
        } else {
            from.deserialize_byte_buf(Payload)
        }
    }
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
mod action_id_tests {
    use super::ActionId;
    use tilo_clock::time::{Duration, UNIX_EPOCH};

    /// The id's time is the clock this process was given, never `std`'s own: an app's model runs
    /// as a WebAssembly module with no `std` clock to ask, and mints its ids under the host's
    /// pinned reading.
    #[test]
    fn an_id_is_minted_at_the_time_the_process_holds() {
        let at = UNIX_EPOCH + Duration::from_millis(1_700_000_000_123);
        let (first, second) =
            tilo_clock::testing::with_wall(at, || (ActionId::new(), ActionId::new()));
        for id in [first, second] {
            let (seconds, nanos) = id.0.get_timestamp().expect("a version 7 id").to_unix();
            assert_eq!(
                seconds * 1_000 + u64::from(nanos) / 1_000_000,
                1_700_000_000_123,
                "the id carries the time the process holds"
            );
        }
        assert!(first.0 < second.0, "an id sorts after the one before it");
    }
}

#[cfg(test)]
mod operation_kind_tests {
    use super::OperationKind;
    use crate::ActionKind;

    /// What a window may report as a change of the person's history: a document or view change,
    /// or a read the input made. An effect changes nothing a history records, so reporting one
    /// would ask the broker to admit a change that does not exist.
    #[test]
    fn only_a_change_or_a_read_is_reported() {
        for (kind, need, reported) in [
            (OperationKind::Document, ActionKind::Undoable, true),
            (OperationKind::Document, ActionKind::Read, true),
            (OperationKind::View, ActionKind::Read, true),
            (OperationKind::View, ActionKind::Outside, true),
            (OperationKind::Effect, ActionKind::Outside, false),
            (OperationKind::Effect, ActionKind::Undoable, false),
            (OperationKind::Effect, ActionKind::Read, true),
        ] {
            assert_eq!(kind.reports_a_change(need), reported, "{kind:?}/{need:?}");
        }
    }

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
