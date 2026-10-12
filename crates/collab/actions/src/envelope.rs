//! The [`ActionEnvelope`]: what actually replicates.
//!
//! Binds an [`EncodedAction`] to the metadata a peer needs to *replay it
//! into an identical document*: the base heads it was authored against,
//! the deterministic actor id ([`ActionId`]), the commit timestamp, and
//! the declared [`ExecutionLocus`]. All `Serialize`/`Deserialize` so it
//! rides any transport (the local broker today, MLS-encrypted Iroh later).

use automerge::{ActorId, ChangeHash};
use serde::{Deserialize, Serialize};

use crate::action::{ActionId, EncodedAction, ExecutionLocus};
use crate::context::ActionContext;

/// One replayable action, ready to cross a boundary.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionEnvelope {
    /// Unique id; its bytes are also the Automerge actor used to commit the
    /// action's change, so replay reproduces the exact same change hash.
    pub id: ActionId,
    /// The serialized action.
    pub action: EncodedAction,
    /// Document heads the action was authored against — a replica at exactly
    /// these heads replays to the identical document.
    pub base_heads: Vec<[u8; 32]>,
    /// Commit timestamp (unix seconds), pinned so the change is
    /// byte-identical on origin and every replaying replica.
    pub timestamp: i64,
    /// The action's declared execution locus, carried so the transport can
    /// honor it.
    pub locus: ExecutionLocus,
    /// The participant that authored this action, when named — a person, an
    /// agent, or another session reached through [`crate::door`].
    ///
    /// These bytes become the Automerge actor the change commits under, so
    /// an edit from outside attributes in history exactly like a typed one.
    /// `None` is the app's own dispatch (the [`ActionId`]-derived actor).
    /// Replay stays byte-identical either way, since the actor rides on the
    /// envelope rather than the applying replica. Optional on the wire so an
    /// envelope written before attribution existed still deserializes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<Vec<u8>>,
    /// Human provenance used by history, search, and passive activity UI.
    /// It does not affect what the action is allowed to mutate; it is carried
    /// into the Automerge change message so it survives the transport log.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<ActionContext>,
}

impl ActionEnvelope {
    /// The content the change's recorded action names
    /// ([`crate::executor::recorded_form`]): the large parts of an action
    /// over the history budget, which the committing device keeps in its
    /// content store so receivers can resolve them to replay. Empty for an
    /// action recorded whole.
    ///
    /// # Errors
    /// A payload that is not JSON.
    pub fn recorded_content(&self) -> Result<crate::content::ContentParts, crate::ActionError> {
        let named = self
            .context
            .as_ref()
            .and_then(|context| context.action.as_ref())
            .map(EncodedAction::content_parts)
            .unwrap_or_default();
        if named.is_empty() {
            return Ok(crate::content::ContentParts::new());
        }
        let (_, content) = self
            .action
            .with_content_parts(crate::content::CONTENT_PART_THRESHOLD)?;
        let mut kept = crate::content::ContentParts::new();
        for hash in &named {
            if let Some(bytes) = content.get(hash) {
                kept.add(bytes.to_vec());
            }
        }
        Ok(kept)
    }

    /// The base heads as Automerge [`ChangeHash`]es.
    pub fn base_change_hashes(&self) -> Vec<ChangeHash> {
        self.base_heads.iter().map(|b| ChangeHash(*b)).collect()
    }

    /// The actor this action's change commits under: the authoring
    /// participant when named, else the [`ActionId`]-derived actor. Read off
    /// the envelope so a replaying replica never substitutes its own.
    pub fn actor(&self) -> ActorId {
        match &self.actor {
            Some(bytes) if !bytes.is_empty() => ActorId::from(bytes.clone()),
            _ => ActorId::from(self.id.actor_bytes().to_vec()),
        }
    }
}
