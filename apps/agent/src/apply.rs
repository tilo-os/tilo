//! The agent's replica of the document, and how it edits it.
//!
//! Edits go through the word processor's exported actions — the very
//! `WordProcessorAction::apply` the toolbar dispatches — applied under the
//! agent's own actor and stamped like the app stamps its commits, so the
//! ops are the app's ops and the author is the agent.

use anyhow::{Context, Result};
use automerge::transaction::Transactable;
use automerge::{ActorId, AutoCommit};
use qos_actions::Action;
use word_processor::actions::WordProcessorAction;

pub struct Editor {
    pub doc: AutoCommit,
    actor: ActorId,
}

impl Editor {
    /// A fresh, unbound document under `actor`. Used by tests; a live
    /// agent always starts from a peer's snapshot.
    pub fn empty(actor_handle: &str) -> Self {
        let actor = ActorId::from(actor_handle.as_bytes());
        let mut doc = AutoCommit::new();
        doc.set_actor(actor.clone());
        Self { doc, actor }
    }

    /// The replica, seeded from a full snapshot a peer sent.
    pub fn from_snapshot(bytes: &[u8], actor_handle: &str) -> Result<Self> {
        let actor = ActorId::from(actor_handle.as_bytes());
        let mut doc = AutoCommit::load(bytes).context("load the document snapshot")?;
        doc.set_actor(actor.clone());
        let mut this = Self { doc, actor };
        this.drain();
        Ok(this)
    }

    /// Merge a full snapshot from a peer. Never replaces: both sides keep
    /// whatever the other had not seen.
    pub fn merge_full(&mut self, bytes: &[u8]) -> Result<()> {
        let mut incoming = AutoCommit::load(bytes).context("load a peer snapshot")?;
        self.doc.merge(&mut incoming).context("merge a peer snapshot")?;
        self.drain();
        Ok(())
    }

    /// Apply a peer's incremental change set.
    pub fn merge_incremental(&mut self, bytes: &[u8]) -> Result<()> {
        self.doc
            .load_incremental(bytes)
            .context("apply a peer's incremental changes")?;
        self.drain();
        Ok(())
    }

    /// Forget what was just merged as far as "changes to broadcast" goes:
    /// `save_incremental` reports everything since the last save, and a
    /// peer's changes must not be echoed back to the room as ours.
    fn drain(&mut self) {
        let _ = self.doc.save_incremental();
    }

    /// Apply one action under the agent's actor and stamp the commit the way
    /// the app does. Returns the incremental bytes to broadcast, or `None`
    /// when the action changed nothing (an unknown block id is a silent
    /// no-op in the app too).
    pub fn apply(&mut self, action: &WordProcessorAction) -> Result<Option<Vec<u8>>> {
        self.doc.set_actor(self.actor.clone());
        action
            .apply(&mut self.doc)
            .map_err(|e| anyhow::anyhow!("apply {action:?}: {e:?}"))?;
        if self.doc.pending_ops() == 0 {
            return Ok(None);
        }
        qdoc::history::commit_stamped(&mut self.doc);
        let delta = self.doc.save_incremental();
        Ok((!delta.is_empty()).then_some(delta))
    }

    /// The whole document, for a peer that just arrived.
    pub fn full(&mut self) -> Vec<u8> {
        let bytes = self.doc.save();
        // `save` resets the incremental baseline too; nothing of ours is
        // pending at this point, so that is harmless.
        bytes
    }

    /// The document as blocks in order.
    pub fn snapshot(&self) -> Result<qdoc::DocSnapshot> {
        let c = qdoc::model::open(&self.doc, &automerge::ROOT)
            .map_err(|e| anyhow::anyhow!("the document has no qdoc schema: {e:?}"))?;
        Ok(qdoc::model::materialize(&self.doc, &c))
    }
}
