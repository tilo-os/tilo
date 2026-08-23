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

/// Whether a peer's snapshot is a document with blocks in it.
///
/// The room's traffic is not all documents, and none of it is labelled:
/// the broker fans a frame out by address, and a participant that
/// announced no app id is cluster-scoped and reaches every subscriber in
/// the room. Seeding a replica from whatever arrived first is how the
/// agent ended up holding the file manager's state and then failing to
/// merge the page it had been invited to.
pub fn looks_like_a_document(bytes: &[u8]) -> bool {
    let Ok(doc) = AutoCommit::load(bytes) else {
        return false;
    };
    has_document_schema(&doc)
}

fn has_document_schema(doc: &AutoCommit) -> bool {
    qdoc::model::open(doc, &automerge::ROOT).is_ok()
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
    ///
    /// Check [`looks_like_a_document`] first: a room hands out snapshots
    /// of everything in it, and this will happily seed itself from
    /// something that is not a document at all.
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
    ///
    /// Returns whether the snapshot was taken. **A room carries more than
    /// one document**, and a subscriber that announced no record — or a
    /// participant that announced no app at all, which the broker treats
    /// as cluster-scoped — is handed all of them. So a snapshot that is
    /// not this document is an ordinary event, not an error: two
    /// unrelated Automerge documents that happen to share an actor id
    /// fail to merge with `duplicate seq N found for actor …`, and
    /// propagating that killed the agent mid-run with a message about
    /// sequence numbers.
    pub fn merge_full(&mut self, bytes: &[u8]) -> Result<bool> {
        let Ok(mut incoming) = AutoCommit::load(bytes) else {
            return Ok(false);
        };
        if !has_document_schema(&incoming) {
            return Ok(false);
        }
        if self.doc.merge(&mut incoming).is_err() {
            return Ok(false);
        }
        self.drain();
        Ok(true)
    }

    /// Whether a peer's snapshot is a *different* document that is asking
    /// for something from us.
    ///
    /// Only consulted once a snapshot has failed to merge, which is how
    /// "a different document" is established. Without a `--record` the
    /// agent cannot tell from the address which of a room's documents is
    /// the one it was invited to, and adopting the first snapshot to
    /// arrive picked whichever app answered first — in practice the file
    /// manager's. An instruction addressed to us by name is the one
    /// signal in the content that settles it.
    pub fn is_addressed_to(bytes: &[u8], name: &str) -> bool {
        let Ok(doc) = AutoCommit::load(bytes) else {
            return false;
        };
        let Ok(c) = qdoc::model::open(&doc, &automerge::ROOT) else {
            return false;
        };
        let snapshot = qdoc::model::materialize(&doc, &c);
        let blocks = crate::plan::blocks(&snapshot);
        crate::plan::find_addressed(&blocks, name).is_some()
    }

    /// Take `bytes` as the document, discarding what we hold.
    ///
    /// Only for the case above: we adopted the wrong document because
    /// nothing in the address distinguished them, and a later one turned
    /// out to be the one addressed to us. Nothing of ours is lost —
    /// this only ever runs before the agent has made an edit.
    pub fn replace_with(&mut self, bytes: &[u8]) -> Result<()> {
        let mut doc = AutoCommit::load(bytes).context("load a peer snapshot")?;
        doc.set_actor(self.actor.clone());
        self.doc = doc;
        self.drain();
        Ok(())
    }

    /// Apply a peer's incremental change set.
    ///
    /// Returns whether it applied. A delta belonging to one of the
    /// room's other documents refers to objects this one has never seen;
    /// that is not this agent's business and must not end its run.
    pub fn merge_incremental(&mut self, bytes: &[u8]) -> Result<bool> {
        if self.doc.load_incremental(bytes).is_err() {
            return Ok(false);
        }
        self.drain();
        Ok(true)
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

    /// How many ancestor changes this replica is waiting for.
    ///
    /// Automerge will not apply a change whose dependencies it has not
    /// got — it queues it — and nothing about reading the document
    /// afterwards says so: it simply materialises as the page *before*
    /// the gap, however many later changes are sitting in the queue. So
    /// a replica that missed one change in the middle reads exactly like
    /// a document that has nothing more in it, and a snapshot saved from
    /// it carries the queued changes along, spreading the same silent
    /// truncation to whoever it is sent to.
    ///
    /// This is the one honest way to tell "there is nothing more" from
    /// "there is more, and I was never sent what it stands on".
    pub fn missing_ancestors(&mut self) -> usize {
        self.doc.get_missing_deps(&[]).len()
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
