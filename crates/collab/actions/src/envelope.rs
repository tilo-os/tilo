//! The [`ActionEnvelope`]: what actually replicates.
//!
//! The envelope binds an [`EncodedAction`] to the metadata a peer needs to
//! *replay it into an identical document*: the base heads it was authored
//! against, the deterministic actor id (its [`ActionId`]), the commit
//! timestamp, and the declared [`ExecutionLocus`]. Everything here is
//! `Serialize`/`Deserialize` so it can ride any transport (the local broker
//! today, MLS-encrypted libp2p later).

use automerge::{ActorId, ChangeHash};
use serde::{Deserialize, Serialize};

use crate::action::{ActionId, EncodedAction, ExecutionLocus};

/// One replayable action, ready to cross a boundary.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionEnvelope {
    /// Unique id; its bytes are also the Automerge actor used to commit the
    /// action's change, so replay reproduces the exact same change hash.
    pub id: ActionId,
    /// The serialized action.
    pub action: EncodedAction,
    /// Document heads the action was authored against. A replica at exactly
    /// these heads replays to the identical document.
    pub base_heads: Vec<[u8; 32]>,
    /// Commit timestamp (unix seconds), pinned so the change is byte-identical
    /// on origin and on every replaying replica.
    pub timestamp: i64,
    /// The action's declared execution locus, carried so the transport can
    /// honor it.
    pub locus: ExecutionLocus,
    /// The participant that authored this action, when it was authored by
    /// someone the app can name — a person, an agent, another session that
    /// reached the app through [`crate::door`].
    ///
    /// These bytes become the Automerge actor the change commits under, so an
    /// edit made from outside is attributed in history exactly like one someone
    /// typed. `None` is the app's own dispatch, keeping the
    /// [`ActionId`]-derived actor. Replay stays byte-identical either way,
    /// because the actor rides on the envelope rather than being supplied by
    /// whichever replica applies it.
    ///
    /// Optional on the wire so an envelope written before attribution existed
    /// still deserializes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<Vec<u8>>,
}

impl ActionEnvelope {
    /// The base heads as Automerge [`ChangeHash`]es.
    pub fn base_change_hashes(&self) -> Vec<ChangeHash> {
        self.base_heads.iter().map(|b| ChangeHash(*b)).collect()
    }

    /// The actor this action's change commits under: the authoring participant
    /// when the envelope names one, else the deterministic actor derived from
    /// [`ActionId`]. Read off the envelope, so a replaying replica never
    /// substitutes its own and the change stays identical to the original.
    pub fn actor(&self) -> ActorId {
        match &self.actor {
            Some(bytes) if !bytes.is_empty() => ActorId::from(bytes.clone()),
            _ => ActorId::from(self.id.actor_bytes().to_vec()),
        }
    }
}
