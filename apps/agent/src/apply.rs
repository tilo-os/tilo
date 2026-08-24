//! The agent's replicas, and how it edits them.
//!
//! Edits go through the apps' own exported actions — never the agent's idea
//! of what a change should be — applied under the agent's own actor and
//! stamped like the app stamps its commits, so the ops are the app's ops and
//! the author is the agent.
//!
//! There are two ways in, and they differ only in who composed the change:
//!
//! * [`Editor::invoke`] performs one operation an app currently exports,
//!   by address, through [`qos_actions::door`]. The address is resolved
//!   against a tree built **now**, so an operation that has gone away since
//!   it was offered is a refusal rather than a substitution.
//! * [`Editor::apply`] performs one word-processor action the agent
//!   composed itself, for the editorial recipes no single exported
//!   operation covers.
//!
//! Both commit under the agent's actor, because that is the whole claim: an
//! edit made this way is attributed in the room's history exactly like an
//! edit somebody typed.

use anyhow::{Context, Result};
use automerge::transaction::Transactable;
use automerge::{ActorId, AutoCommit};
use qos_actions::door::{self, ActionAddress, DoorError};
use qos_actions::{ActionRegistry, LocalTransport, Resource};

use crate::surface::{AppContract, View};

pub struct Editor {
    pub doc: AutoCommit,
    actor: ActorId,
    /// What the room calls whoever is acting through this replica.
    ///
    /// Stamped onto the text every edit adds, as the durable `author` mark, so
    /// a reader an hour later can still tell which paragraphs came from here.
    /// The actor id is the same string hashed, but an actor is an opaque
    /// replica handle: it is not a name, and nothing reading a document is
    /// entitled to turn one back into the other.
    author: String,
    /// Which app authored this document — the contract that builds its
    /// resource tree and registers its decoders.
    contract: &'static AppContract,
}

/// Every block's text, keyed by block id, flattened across table cells and any
/// other nesting the snapshot reports.
///
/// The before-half of authorship: what an action *added* is the difference
/// between this and the same reading afterwards, which works for every action
/// rather than only the ones whose arguments happen to name a range.
fn block_texts(doc: &AutoCommit) -> std::collections::BTreeMap<String, String> {
    let mut out = std::collections::BTreeMap::new();
    let Ok(c) = qdoc::model::open(doc, &automerge::ROOT) else {
        return out;
    };
    let snap = qdoc::model::materialize(doc, &c);
    fn walk(blocks: &[qdoc::BlockView], out: &mut std::collections::BTreeMap<String, String>) {
        for b in blocks {
            out.insert(b.id.clone(), b.text.clone());
            walk(&b.children, out);
        }
    }
    walk(&snap.blocks, &mut out);
    out
}

/// The character range `after` added relative to `before`, as `[start, end)`.
///
/// Measured as "the middle that differs": the common prefix and common suffix
/// are whatever both readings share, so text inserted into the middle of an
/// existing paragraph is attributed to whoever inserted it without
/// re-attributing the sentence it landed inside. `None` when nothing grew,
/// which covers a pure deletion and a no-op alike — neither authors any text.
fn added_range(before: &str, after: &str) -> Option<(usize, usize)> {
    let b: Vec<char> = before.chars().collect();
    let a: Vec<char> = after.chars().collect();
    if a.len() <= b.len() {
        return None;
    }
    let mut head = 0;
    while head < b.len() && b[head] == a[head] {
        head += 1;
    }
    let mut tail = 0;
    while tail < b.len() - head && b[b.len() - 1 - tail] == a[a.len() - 1 - tail] {
        tail += 1;
    }
    let end = a.len() - tail;
    (end > head).then_some((head, end))
}

/// Which app authored a peer's snapshot, or `None` when no app the agent
/// speaks to claims it.
///
/// The room's traffic is not all one app's, and none of it is labelled: the
/// broker fans a frame out by address, and a participant that announced no
/// app id is cluster-scoped and reaches every subscriber in the room.
/// Seeding a replica from whatever arrived first is how the agent ended up
/// holding the file manager's state and then failing to merge the page it
/// had been invited to. Asking each app whether the bytes are its own is
/// what tells them apart — and the same answer says which contract can
/// build a tree for it.
pub fn attribute(bytes: &[u8]) -> Option<&'static AppContract> {
    let doc = AutoCommit::load(bytes).ok()?;
    crate::surface::recognise(&doc)
}

/// Whether a peer's snapshot is a document with blocks in it.
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
        Self::empty_for(&crate::surface::CONTRACTS[0], actor_handle)
    }

    /// [`Editor::empty`] for a named app.
    pub fn empty_for(contract: &'static AppContract, actor_handle: &str) -> Self {
        let actor = ActorId::from(actor_handle.as_bytes());
        let mut doc = AutoCommit::new();
        doc.set_actor(actor.clone());
        Self {
            doc,
            actor,
            author: actor_handle.to_string(),
            contract,
        }
    }

    /// Which app authored the document this holds.
    pub fn contract(&self) -> &'static AppContract {
        self.contract
    }

    /// The tree the app publishes for this document, seen from `view`.
    ///
    /// Rebuilt on every call. An app's tree changes with every edit, and an
    /// address is only worth anything if it names what is there now.
    pub fn resources(&self, view: &View) -> Vec<Resource> {
        crate::surface::tree(self.contract, view, &self.doc)
    }

    /// The replica, seeded from a full snapshot a peer sent.
    ///
    /// Check [`looks_like_a_document`] first: a room hands out snapshots
    /// of everything in it, and this will happily seed itself from
    /// something that is not a document at all.
    pub fn from_snapshot(bytes: &[u8], actor_handle: &str) -> Result<Self> {
        let contract = attribute(bytes)
            .context("no app the agent speaks to claims this document")?;
        Self::from_snapshot_for(contract, bytes, actor_handle)
    }

    /// [`Editor::from_snapshot`] for an already-attributed snapshot.
    pub fn from_snapshot_for(
        contract: &'static AppContract,
        bytes: &[u8],
        actor_handle: &str,
    ) -> Result<Self> {
        let actor = ActorId::from(actor_handle.as_bytes());
        let mut doc = AutoCommit::load(bytes).context("load the document snapshot")?;
        doc.set_actor(actor.clone());
        let mut this = Self {
            doc,
            actor,
            author: actor_handle.to_string(),
            contract,
        };
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

    /// Merge a full snapshot this app authored, whatever its schema.
    ///
    /// [`Editor::merge_full`] insists the incoming bytes carry a block
    /// document, which is right for the page and wrong for every other app
    /// in the room: a file manager's state has no blocks in it and would be
    /// refused as "not a document" by a test that only knows one shape.
    /// Here the app itself has already said the bytes are its own.
    pub fn merge_full_any(&mut self, bytes: &[u8]) -> Result<bool> {
        let Ok(mut incoming) = AutoCommit::load(bytes) else {
            return Ok(false);
        };
        if !(self.contract.recognises)(&incoming) {
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
    ///
    /// **Whether it applied cannot be read off the error.** Automerge does
    /// not reject a change whose ancestors are missing — it *queues* it and
    /// reports success. On a feed that carries every application's frames,
    /// and the agent's does by design, taking that at its word goes wrong
    /// twice over. It reports an edit that did not happen, so the agent
    /// surveys and decides on somebody else's keystroke; and because this
    /// replica is asked first and its answer is what stops the search, the
    /// delta never reaches the replica it did belong to, which then quietly
    /// goes stale. It also parks a dependency that can never be met, and
    /// `missing_ancestors` is read straight off `get_missing_deps` — one
    /// foreign delta and the agent reports for the rest of its run that the
    /// page it holds is short.
    ///
    /// So the ancestors are checked against this replica before anything
    /// touches it. On a throwaway load of the delta rather than on a copy of
    /// the document, because the delta is one edit and the document is a
    /// whole page.
    pub fn merge_incremental(&mut self, bytes: &[u8]) -> Result<bool> {
        let mut incoming = AutoCommit::new();
        if incoming.load_incremental(bytes).is_err() {
            return Ok(false);
        }
        // What the delta is built on that it does not carry itself. Every one
        // of those has to be history this replica already holds, or applying
        // it would park it rather than apply it.
        if !incoming
            .get_missing_deps(&[])
            .iter()
            .all(|h| self.doc.get_change_by_hash(h).is_some())
        {
            return Ok(false);
        }
        let before = self.doc.get_heads();
        if self.doc.load_incremental(bytes).is_err() {
            return Ok(false);
        }
        self.drain();
        // The document moving is the claim being made, so it is the one
        // measured. A delta this replica already had moves nothing and is
        // not news.
        Ok(self.doc.get_heads() != before)
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
    ///
    /// **Whether to broadcast is decided on the heads, not on `pending_ops`.**
    /// `AutoCommit` closes an open transaction to answer a good many of its
    /// own methods — `get_heads`, `save_incremental`, `fork`, `merge`,
    /// `diff_incremental` — so an action that reaches for any of them between
    /// its writes has already committed by the time it returns, and leaves
    /// nothing pending. A guard on `pending_ops` reads that as "this changed
    /// nothing" and returns before `save_incremental` ever runs: the operation
    /// is performed on the local replica, no error is raised, and the change
    /// is never sent. The agent then believes it answered and no other machine
    /// ever saw it.
    ///
    /// It fails only on the success path, which is what makes it hard to
    /// notice: a refusal writes one splice with nothing interleaved and
    /// travels fine. `pending_ops` answers a question about an open
    /// transaction; the question here is whether the document moved, so that
    /// is the one asked.
    ///
    /// Generic over the action rather than fixed to
    /// [`WordProcessorAction`] so this guard can be exercised with an action
    /// of the shape that breaks it. Every caller still passes the word
    /// processor's own.
    pub fn apply<A>(&mut self, action: &A) -> Result<Option<Vec<u8>>>
    where
        A: qos_actions::Action + std::fmt::Debug + ?Sized,
    {
        self.doc.set_actor(self.actor.clone());
        // A clean commit boundary and the baseline to compare against:
        // `get_heads` closes any open transaction, so what follows belongs to
        // this action alone.
        let before = self.doc.get_heads();
        let text_before = block_texts(&self.doc);
        action
            .apply(&mut self.doc)
            .map_err(|e| anyhow::anyhow!("apply {action:?}: {e:?}"))?;
        self.stamp_authorship(&text_before);
        // Stamp only what is still open. An action whose own read closed the
        // transaction has already committed and has nothing left to stamp.
        if self.doc.pending_ops() > 0 {
            qdoc::history::commit_stamped(&mut self.doc);
        }
        if self.doc.get_heads() == before {
            return Ok(None);
        }
        let delta = self.doc.save_incremental();
        Ok((!delta.is_empty()).then_some(delta))
    }

    /// Apply an action belonging to some app other than the word processor —
    /// the agent answering in a channel — under the agent's own actor.
    ///
    /// Separate from [`Self::apply`] because of what it does *not* do: no
    /// `qdoc` history stamp. That stamp writes the word processor's own
    /// undo/authorship schema, and putting it on another app's document would
    /// be this agent inventing structure in a document it does not own.
    ///
    /// The actor still is the agent's, so the message is attributed to it in
    /// the room exactly as its edits are.
    pub fn apply_foreign<A: qos_actions::Action>(
        &mut self,
        action: &A,
    ) -> Result<Option<Vec<u8>>> {
        self.doc.set_actor(self.actor.clone());
        action
            .apply(&mut self.doc)
            .map_err(|e| anyhow::anyhow!("apply a foreign action: {e:?}"))?;
        // No `pending_ops` short-circuit here, deliberately. `AutoCommit`
        // commits what is pending whenever a read interleaves the writes, and
        // an app's own writer reads as it goes — chat's message insert asks
        // the list its length between two writes. So by the time control
        // returns, the ops are usually already committed and `pending_ops` is
        // zero, which said "this changed nothing" about a message that had in
        // fact been written. The agent then held an answer it had never sent.
        //
        // What actually settles it is whether there are unsaved changes, so
        // that is what is asked.
        self.doc.commit();
        let delta = self.doc.save_incremental();
        Ok((!delta.is_empty()).then_some(delta))
    }

    /// Perform the operation `address` names, attributed to the agent.
    ///
    /// The address is resolved against a tree built from the document **as
    /// it stands right now**, not against the survey it was chosen from.
    /// That is the whole of the dispatch-time check: a section that has been
    /// deleted, a file that has been renamed, an operation the app has
    /// stopped publishing — each is [`DoorError::Unknown`] here, and nothing
    /// is dispatched. The alternative, resolving against a remembered tree,
    /// would perform an operation on whatever now happens to carry that id.
    ///
    /// `registry` is the apps' own decoders: the door decodes the encoded
    /// action through it before running anything, so an action whose type no
    /// app registered is refused rather than applied on trust.
    ///
    /// Returns the incremental bytes to broadcast, or `None` when the
    /// operation changed nothing (a no-op is ordinary — a rename proposal
    /// that already reads that way writes nothing). Whether it changed
    /// anything is read off what is left unsaved, never off `pending_ops`:
    /// an app's own writer reads as it goes, and by then `AutoCommit` has
    /// committed the very change that would not be sent. See
    /// [`Self::apply`].
    pub fn invoke(
        &mut self,
        view: &View,
        address: &ActionAddress,
        argument: Option<&str>,
        registry: &ActionRegistry,
    ) -> Result<Option<Vec<u8>>, DoorError> {
        let resources = self.resources(view);
        let mut transport = LocalTransport::new();
        self.doc.set_actor(self.actor.clone());
        let text_before = block_texts(&self.doc);
        door::invoke(
            &mut self.doc,
            &resources,
            address,
            argument,
            self.actor.to_bytes(),
            registry,
            &mut transport,
        )?;
        // The same stamp the direct path applies. An operation invoked through
        // the door is still this agent writing into somebody's document, and
        // authorship that depended on which entrance was used would be a fact
        // about the plumbing rather than about who wrote the sentence.
        self.stamp_authorship(&text_before);
        let delta = self.doc.save_incremental();
        Ok((!delta.is_empty()).then_some(delta))
    }

    /// Mark whatever text this replica just added with the durable `author`
    /// mark, so the document itself records who wrote it.
    ///
    /// Read back by [`qdoc::author_runs`] and drawn by the word processor. It
    /// is a mark rather than a field on the block because authorship follows
    /// the characters: splitting a paragraph, merging two, or typing into the
    /// middle of somebody else's sentence all leave each run attributed to
    /// whoever actually wrote it.
    ///
    /// Only text that *grew* is stamped. A deletion authors nothing, and an
    /// action that rewrote a block's attributes without touching its text has
    /// not made anybody the author of the sentence it left alone.
    fn stamp_authorship(&mut self, before: &std::collections::BTreeMap<String, String>) {
        if self.author.is_empty() {
            return;
        }
        let after = block_texts(&self.doc);
        let mut spans: Vec<(String, usize, usize)> = Vec::new();
        for (id, text) in &after {
            let was = before.get(id).map(String::as_str).unwrap_or("");
            if let Some((start, end)) = added_range(was, text) {
                spans.push((id.clone(), start, end));
            }
        }
        if spans.is_empty() {
            return;
        }
        let Ok(c) = qdoc::model::open(&self.doc, &automerge::ROOT) else {
            return;
        };
        let mut ids = qdoc::IdGen::new(self.doc.get_actor().to_hex_string());
        let mut dm = qdoc::DocMut {
            doc: &mut self.doc,
            c: &c,
            ids: &mut ids,
        };
        for (id, start, end) in spans {
            dm.add_mark(&id, start, end, qdoc::schema::MARK_AUTHOR, self.author.as_str());
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::{ReadDoc, ROOT, ScalarValue};
    use qos_actions::{Action, ActionError, Doc, ExecutionLocus};

    /// An action that asks the document a question between its writes.
    ///
    /// `AutoCommit` closes an open transaction to answer `get_heads` — as it
    /// does for `save_incremental`, `fork`, `merge` and `diff_incremental` —
    /// so everything written above is already a committed change here, and
    /// nothing is left pending when the action returns. That is the condition
    /// a `pending_ops` guard gets wrong, and any writer that checks where it
    /// is mid-action produces it.
    #[derive(Debug)]
    struct ReadsAfterWriting;

    impl Action for ReadsAfterWriting {
        fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
            use automerge::transaction::Transactable;
            doc.put(&ROOT, "note", ScalarValue::Str("written".into()))?;
            // The question that closes the transaction.
            let _ = doc.get_heads();
            Ok(())
        }
        fn locus(&self) -> ExecutionLocus {
            ExecutionLocus::ClientLocal
        }
    }

    /// An action that genuinely does nothing — an unknown block id, in the
    /// app. It must stay silent.
    #[derive(Debug)]
    struct Nothing;

    impl Action for Nothing {
        fn apply(&self, _doc: &mut Doc) -> Result<(), ActionError> {
            Ok(())
        }
        fn locus(&self) -> ExecutionLocus {
            ExecutionLocus::ClientLocal
        }
    }

    /// Another app's document in the same room, and one later change to it.
    ///
    /// Shaped like the file explorer's: a different schema under a different
    /// actor, nothing the word processor has ever seen. Returns the delta the
    /// broker fans out for that later change — which names the first one as an
    /// ancestor, so a replica that never had the first cannot apply it.
    fn a_delta_from_another_apps_document() -> Vec<u8> {
        use automerge::ObjType;
        let mut foreign = AutoCommit::new();
        foreign.set_actor(ActorId::from("xaver".as_bytes()));
        let buffer = foreign
            .put_object(&ROOT, "files.rename_query", ObjType::Text)
            .expect("seed the other app's document");
        foreign.commit();
        // Everything so far is the document that peer already had; only what
        // follows is on the wire.
        let _ = foreign.save_incremental();
        foreign.splice_text(&buffer, 0, 0, "launch.md").expect("type into it");
        foreign.commit();
        foreign.save_incremental()
    }

    /// A delta belonging to another app's document is refused, and refusing
    /// it leaves nothing behind.
    ///
    /// The agent joins with an empty app id by design, so the broker hands it
    /// every application's frames and this delta is tried against the page as
    /// a matter of course. Automerge does not reject a change whose ancestors
    /// are missing — it *queues* it — so taking it on trust both reports an
    /// edit that did not happen and parks a dependency that will never be
    /// met. The second half is the one that does lasting damage:
    /// `missing_ancestors` is read straight off `get_missing_deps`, and it is
    /// what makes the agent announce that the page it holds is not whole.
    #[test]
    fn a_delta_from_another_document_is_refused_and_leaves_nothing_queued() {
        let delta = a_delta_from_another_apps_document();

        // The condition, checked rather than assumed: this delta really is
        // one automerge accepts-by-queueing rather than one it rejects. If
        // that ever changed, the guard below would be proving nothing.
        {
            let mut probe = AutoCommit::new();
            assert!(
                probe.load_incremental(&delta).is_ok(),
                "automerge must accept this delta, or the trap this guards \
                 against does not exist"
            );
            assert!(
                !probe.get_missing_deps(&[]).is_empty(),
                "the delta must be one whose ancestor is missing, or this \
                 exercises nothing"
            );
        }

        let mut ed = Editor::empty("Scribe");
        let before = ed.missing_ancestors();
        assert!(
            !ed.merge_incremental(&delta).expect("merge"),
            "a delta belonging to another document is not this one's, and \
             saying it applied reports an edit that did not happen"
        );
        assert_eq!(
            ed.missing_ancestors(),
            before,
            "refusing a foreign delta must leave nothing queued behind it — \
             a parked dependency that can never be met makes the agent \
             report for the rest of its run that the page it holds is short"
        );
    }

    /// A change whose action read after writing is still broadcast.
    ///
    /// The condition is asserted, not assumed: the probe is run against a
    /// bare document first to show that it really does leave nothing
    /// pending. Without that line this test would pass against any guard at
    /// all, including the one it exists to catch.
    #[test]
    fn a_change_is_sent_even_when_the_action_read_after_writing() {
        let mut probe = AutoCommit::new();
        ReadsAfterWriting.apply(&mut probe).expect("the probe applies");
        assert_eq!(
            probe.pending_ops(),
            0,
            "the probe must leave nothing pending, or this test is not \
             exercising the condition it is about"
        );

        let mut ed = Editor::empty("Scribe");
        let delta = ed
            .apply(&ReadsAfterWriting)
            .expect("apply")
            .expect("the document changed, so there is a change to send");

        // What matters is the other machine: a peer that only ever sees the
        // delta has to land on the same document.
        let mut peer = AutoCommit::new();
        peer.load_incremental(&delta).expect("the delta applies");
        assert_eq!(
            peer.get(&ROOT, "note")
                .unwrap()
                .and_then(|(v, _)| v.to_str().map(str::to_string)),
            Some("written".to_string()),
            "the peer never received the change the agent made"
        );
    }

    /// And the silence is still honest: an action that changes nothing
    /// broadcasts nothing, rather than an empty change every peer has to
    /// merge.
    #[test]
    fn an_action_that_changes_nothing_sends_nothing() {
        let mut ed = Editor::empty("Scribe");
        assert_eq!(ed.apply(&Nothing).expect("apply"), None);
    }

    /// The ordinary path is unchanged: an action that leaves its ops pending
    /// is committed, stamped, and sent.
    #[test]
    fn an_ordinary_write_is_still_committed_and_sent() {
        #[derive(Debug)]
        struct JustWrites;
        impl Action for JustWrites {
            fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
                use automerge::transaction::Transactable;
                doc.put(&ROOT, "note", ScalarValue::Str("typed".into()))?;
                Ok(())
            }
            fn locus(&self) -> ExecutionLocus {
                ExecutionLocus::ClientLocal
            }
        }
        let mut ed = Editor::empty("Scribe");
        let delta = ed
            .apply(&JustWrites)
            .expect("apply")
            .expect("a plain write is a change to send");
        let mut peer = AutoCommit::new();
        peer.load_incremental(&delta).expect("the delta applies");
        assert_eq!(
            peer.get(&ROOT, "note")
                .unwrap()
                .and_then(|(v, _)| v.to_str().map(str::to_string)),
            Some("typed".to_string())
        );
    }
}
