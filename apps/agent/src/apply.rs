//! The agent's replicas, and how it edits them.
//!
//! Edits go through the apps' own exported actions — never the agent's idea
//! of what a change should be — applied under the agent's own actor and
//! stamped like the app stamps its commits, so the ops are the app's ops and
//! the author is the agent.
//!
//! Two ways in, differing only in who composed the change: [`Editor::invoke`]
//! performs one operation an app currently exports, by address, through
//! [`tilo_actions::door`], resolved against a tree built **now** so an
//! operation that has gone away since it was offered is a refusal rather
//! than a substitution; [`Editor::apply`] performs one action the agent
//! composed itself.

use anyhow::{Context, Result};
use automerge::transaction::Transactable;
use automerge::{ActorId, AutoCommit};
use tilo_actions::door::{self, ActionAddress, DoorError, InvocationProvenance};
use tilo_actions::{ActionRegistry, LocalTransport, Resource};
use tilo_automerge::ApplyIncoming;

use crate::surface::{AppContract, View};

pub struct Editor {
    pub doc: AutoCommit,
    actor: ActorId,
    /// What the room calls whoever is acting through this replica. Stamped
    /// onto text the agent adds, as the durable `author` mark. The actor id
    /// is the same string hashed, but an actor is an opaque replica handle
    /// and nothing reading a document is entitled to turn one back into a
    /// name.
    author: String,
    /// Durable task provenance applied to native edits while a session is
    /// carrying out one addressed instruction.
    history_context: Option<tilo_actions::ActionContext>,
    /// Which app authored this document.
    contract: &'static AppContract,
    /// Exact authenticated route that supplied this replica. It can differ
    /// from the application definition `contract` comes from.
    route_app_id: String,
}

/// Every block's text, keyed by block id, flattened across nesting.
///
/// The before-half of authorship: what an action *added* is the difference
/// between this and the same reading afterwards, which works for every
/// action rather than only ones whose arguments happen to name a range.
fn block_texts(doc: &AutoCommit) -> std::collections::BTreeMap<String, String> {
    let mut out = std::collections::BTreeMap::new();
    let Ok(c) = tilo_doc::model::open(doc, &automerge::ROOT) else {
        return out;
    };
    let snap = tilo_doc::model::materialize(doc, &c);
    walk_block_texts(&snap.blocks, &mut out);
    out
}

/// Depth-first text collection for [`block_texts`], walked as its own
/// function rather than nested so the item does not follow statements in
/// its caller's scope.
fn walk_block_texts(
    blocks: &[tilo_doc::BlockView],
    out: &mut std::collections::BTreeMap<String, String>,
) {
    for b in blocks {
        out.insert(b.id.clone(), b.text.clone());
        walk_block_texts(&b.children, out);
    }
}

/// The character range `after` added relative to `before`, as `[start, end)`.
///
/// The common prefix and suffix are whatever both readings share, so text
/// inserted into the middle of an existing paragraph is attributed without
/// re-attributing the sentence it landed inside. `None` when nothing grew —
/// a pure deletion and a no-op alike author no text.
fn added_range(before: &str, after: &str) -> Option<(usize, usize)> {
    let b: Vec<char> = before.chars().collect();
    let a: Vec<char> = after.chars().collect();
    if a.len() <= b.len() {
        return None;
    }
    let mut head = 0;
    while head < b.len() && b.get(head) == a.get(head) {
        head += 1;
    }
    let after_head = b.get(head..).unwrap_or(&[]);
    let mut tail = 0;
    for (&x, &y) in after_head.iter().rev().zip(a.iter().rev()) {
        if x != y {
            break;
        }
        tail += 1;
    }
    let end = a.len() - tail;
    (end > head).then_some((head, end))
}

/// Which app authored a peer's snapshot, or `None` when no app the agent
/// speaks to claims it.
///
/// A room's traffic is not all one app's, and none of it is labelled: a
/// participant that announced no app id reaches every subscriber. Asking
/// each app whether the bytes are its own is what tells them apart, and the
/// same answer says which contract can build a tree for it.
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
    tilo_doc::model::open(doc, &automerge::ROOT).is_ok()
}

impl Editor {
    /// A fresh, unbound document under `actor`. Used by tests; a live agent
    /// always starts from a peer's snapshot.
    pub fn empty(actor_handle: &str) -> Self {
        Self::empty_for(&crate::surface::WORD_PROCESSOR, actor_handle)
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
            history_context: None,
            contract,
            route_app_id: contract.app_id.to_string(),
        }
    }

    /// Which app authored the document this holds.
    pub fn contract(&self) -> &'static AppContract {
        self.contract
    }

    /// Exact app route that supplied this replica.
    pub fn route_app_id(&self) -> &str {
        &self.route_app_id
    }

    /// Set task context for subsequent commits. The session clears it after
    /// the run so a later, unrelated instruction cannot inherit it.
    pub fn set_history_context(&mut self, context: Option<tilo_actions::ActionContext>) {
        self.history_context = context;
    }

    /// Stable identity of this replica's author.
    pub fn actor_hex(&self) -> String {
        self.actor.to_hex_string()
    }

    /// Newest non-genesis revision, suitable as one end of a run step.
    pub fn latest_entry_id(&mut self) -> Option<ui::HistoryEntryId> {
        tilo_resource::history::versions(&mut self.doc)
            .into_iter()
            .rev()
            .find(|version| !tilo_doc::model::is_schema_genesis(version))
            .map(|version| ui::history_source::entry_id_from_hash(&version.hash.to_string()))
    }

    /// The tree the app publishes for this document, seen from `view`.
    /// Rebuilt on every call, since an address only means something if it
    /// names what is there now.
    pub fn resources(&self, view: &View) -> Vec<Resource> {
        crate::surface::tree(self.contract, view, &self.doc)
    }

    /// The replica, seeded from a full snapshot a peer sent. Check
    /// [`looks_like_a_document`] first: a room hands out snapshots of
    /// everything in it, and this will happily seed itself from something
    /// that is not a document at all.
    ///
    /// # Errors
    ///
    /// When no app the agent speaks to claims `bytes` ([`attribute`]), or
    /// when `bytes` do not decode as an Automerge document.
    pub fn from_snapshot(bytes: &[u8], actor_handle: &str) -> Result<Self> {
        let contract =
            attribute(bytes).context("no app the agent speaks to claims this document")?;
        Self::from_snapshot_for(contract, bytes, actor_handle)
    }

    /// [`Editor::from_snapshot`] for an already-attributed snapshot, held as
    /// a *peer* document rather than the page the agent was invited to.
    ///
    /// The difference is the Automerge actor, and it is not cosmetic. An
    /// actor id names a **replica**, and the agent holds one replica per
    /// document in the room — sharing one id across documents means two
    /// unrelated ones both carry a change numbered `(agent, 1)`. Nothing is
    /// wrong with that until a cluster-scoped feed hands one document a
    /// change belonging to another: it records that sequence number as
    /// taken, and the real next change for that document is rejected —
    /// `duplicate seq 1 found for actor …` — dropped without a word.
    ///
    /// The page keeps the plain name ([`Editor::from_snapshot`]) because
    /// that is the history somebody reads: the durable `author` mark this
    /// stamps is the plain name either way, only the replica handle differs.
    ///
    /// # Errors
    ///
    /// When `bytes` do not decode as an Automerge document.
    pub fn peer_from_snapshot(
        contract: &'static AppContract,
        bytes: &[u8],
        actor_handle: &str,
    ) -> Result<Self> {
        Self::peer_from_snapshot_on_route(contract, contract.app_id, bytes, actor_handle)
    }

    /// Build a peer replica while retaining the exact authenticated route
    /// that delivered it.
    ///
    /// # Errors
    ///
    /// Returns an error when `bytes` is not a loadable snapshot of the
    /// contract's document.
    pub fn peer_from_snapshot_on_route(
        contract: &'static AppContract,
        route_app_id: &str,
        bytes: &[u8],
        actor_handle: &str,
    ) -> Result<Self> {
        Self::from_snapshot_as(
            contract,
            route_app_id,
            bytes,
            actor_handle,
            &format!("{actor_handle}#{}", contract.namespace),
        )
    }

    /// [`Editor::from_snapshot`] for an already-attributed snapshot.
    ///
    /// # Errors
    ///
    /// When `bytes` do not decode as an Automerge document.
    pub fn from_snapshot_for(
        contract: &'static AppContract,
        bytes: &[u8],
        actor_handle: &str,
    ) -> Result<Self> {
        Self::from_snapshot_as(contract, contract.app_id, bytes, actor_handle, actor_handle)
    }

    /// The two above, with the replica's actor spelled out. `author` is who
    /// the room calls this participant and never changes; `actor` is which
    /// replica is writing, and is per-document.
    fn from_snapshot_as(
        contract: &'static AppContract,
        route_app_id: &str,
        bytes: &[u8],
        actor_handle: &str,
        actor_id: &str,
    ) -> Result<Self> {
        let actor = ActorId::from(actor_id.as_bytes());
        let mut doc = AutoCommit::load(bytes).context("load the document snapshot")?;
        doc.set_actor(actor.clone());
        let mut this = Self {
            doc,
            actor,
            author: actor_handle.to_string(),
            history_context: None,
            contract,
            route_app_id: route_app_id.to_string(),
        };
        this.drain();
        Ok(this)
    }

    /// Merge a full snapshot from a peer. Never replaces: both sides keep
    /// whatever the other had not seen.
    ///
    /// Returns whether the snapshot was taken. A room carries more than one
    /// document, and a subscriber that announced no record — or no app at
    /// all, which is cluster-scoped — is handed all of them, so a snapshot
    /// that is not this document is ordinary rather than an error: two
    /// unrelated documents sharing an actor id fail to merge with
    /// `duplicate seq N found for actor …`, and propagating that killed the
    /// agent mid-run.
    ///
    /// # Errors
    ///
    /// Never returns `Err`: every way `bytes` can fail to become part of
    /// this document — undecodable, the wrong schema, a merge Automerge
    /// itself refuses — becomes `Ok(false)`, which is the point of the
    /// doc comment above. The `Result` shape matches
    /// [`Editor::merge_incremental`], the other half of this pair.
    pub fn merge_full(&mut self, bytes: &[u8]) -> Result<bool> {
        let Ok(mut incoming) = AutoCommit::load(bytes) else {
            return Ok(false);
        };
        if !has_document_schema(&incoming) {
            return Ok(false);
        }
        if self.doc.merge_incoming(&mut incoming).is_err() {
            return Ok(false);
        }
        self.drain();
        Ok(true)
    }

    /// Merge a full snapshot this app authored, whatever its schema.
    ///
    /// [`Editor::merge_full`] insists the incoming bytes carry a block
    /// document, which is wrong for every app whose document has no blocks
    /// in it. Here the app itself has already said the bytes are its own.
    ///
    /// A refusal *after* that gate is logged, unlike the one in
    /// [`Editor::merge_full`]. There, a snapshot that will not merge is
    /// ordinarily just a different document. Here the contract has already
    /// said these bytes are this app's, so a merge that fails anyway is the
    /// room's own state failing to reach the agent, which then sits waiting
    /// to be spoken to about something it will never see.
    ///
    /// # Errors
    ///
    /// Never returns `Err`: a snapshot that will not merge is logged and
    /// reported as `Ok(false)` rather than propagated, for the reason given
    /// above.
    pub fn merge_full_any(&mut self, bytes: &[u8]) -> Result<bool> {
        let Ok(mut incoming) = AutoCommit::load(bytes) else {
            return Ok(false);
        };
        if !(self.contract.recognises)(&incoming) {
            return Ok(false);
        }
        if let Err(e) = self.doc.merge_incoming(&mut incoming) {
            tracing::warn!(
                app = self.contract.namespace,
                bytes = bytes.len(),
                "this app's own state would not merge into the replica held \
                 for it, so the agent cannot see what the room is doing \
                 there: {e}",
            );
            return Ok(false);
        }
        self.drain();
        Ok(true)
    }

    /// Whether a peer's snapshot is a *different* document that is asking
    /// for something from us.
    ///
    /// Only consulted once a snapshot has failed to merge. Without a
    /// `--record` the agent cannot tell from the address which of a room's
    /// documents is the one it was invited to, and adopting the first to
    /// arrive picks whichever app answered first. An instruction addressed
    /// to us by name is the one signal in the content that settles it.
    pub fn is_addressed_to(bytes: &[u8], name: &str) -> bool {
        let Ok(doc) = AutoCommit::load(bytes) else {
            return false;
        };
        let Ok(c) = tilo_doc::model::open(&doc, &automerge::ROOT) else {
            return false;
        };
        let snapshot = tilo_doc::model::materialize(&doc, &c);
        let blocks = crate::plan::blocks(&snapshot);
        crate::plan::find_addressed(&blocks, name).is_some()
    }

    /// Take `bytes` as the document, discarding what we hold. Only for the
    /// case above, and only before the agent has made an edit.
    ///
    /// # Errors
    ///
    /// When `bytes` do not decode as an Automerge document.
    pub fn replace_with(&mut self, bytes: &[u8]) -> Result<()> {
        let mut doc = AutoCommit::load(bytes).context("load a peer snapshot")?;
        doc.set_actor(self.actor.clone());
        self.doc = doc;
        self.drain();
        Ok(())
    }

    /// Apply a peer's incremental change set. Returns whether it applied —
    /// a delta belonging to one of the room's other documents refers to
    /// objects this one has never seen, and that is not this agent's
    /// business.
    ///
    /// **Whether it applied cannot be read off the error.** Automerge does
    /// not reject a change whose ancestors are missing — it *queues* it and
    /// reports success. On a feed that carries every application's frames,
    /// taking that at its word means this replica reports an edit that did
    /// not happen and parks a dependency that can never be met, so
    /// `missing_ancestors` (read off `get_missing_deps`) says for the rest
    /// of the run that the page held is short. So the ancestors are checked
    /// against this replica, on a throwaway load of the delta, before
    /// anything touches it.
    ///
    /// # Errors
    ///
    /// Never returns `Err`: an undecodable delta, or one whose ancestors
    /// this replica is missing, is reported as `Ok(false)` rather than
    /// propagated — see above for why applying it anyway would be wrong.
    pub fn merge_incremental(&mut self, bytes: &[u8]) -> Result<bool> {
        let mut incoming = AutoCommit::new();
        if incoming.load_incoming(bytes).is_err() {
            return Ok(false);
        }
        if !incoming
            .get_missing_deps(&[])
            .iter()
            .all(|h| self.doc.get_change_by_hash(h).is_some())
        {
            return Ok(false);
        }
        let before = self.doc.get_heads();
        if self.doc.load_incoming(bytes).is_err() {
            return Ok(false);
        }
        self.drain();
        // The document moving is the claim being made, so it is the one
        // measured — a delta this replica already had moves nothing and is
        // not news.
        Ok(self.doc.get_heads() != before)
    }

    /// Forget what was just merged as far as "changes to broadcast" goes:
    /// `save_incremental` reports everything since the last save, and a
    /// peer's changes must not be echoed back to the room as ours.
    fn drain(&mut self) {
        let _ = self.doc.save_incremental();
    }

    /// Apply one action under the agent's actor and stamp the commit the
    /// way the app does. Returns the incremental bytes to broadcast, or
    /// `None` when the action changed nothing.
    ///
    /// **Whether to broadcast is decided on the heads, not on
    /// `pending_ops`.** `AutoCommit` closes an open transaction to answer a
    /// good many of its own methods — `get_heads`, `save_incremental`,
    /// `fork`, `merge`, `diff_incremental` — so an action that calls one of
    /// them between its writes has already committed by the time it
    /// returns, and a `pending_ops` guard reads that as "nothing changed"
    /// and never sends it. It fails only on the success path, which is what
    /// makes it hard to notice.
    ///
    /// Generic over the action rather than fixed to one app's, so this
    /// guard can be exercised with an action of the shape that breaks it.
    ///
    /// # Errors
    ///
    /// When `action.apply` fails against this document — for instance, the
    /// action names a block, table or attribute that is no longer there.
    pub fn apply<A>(&mut self, action: &A) -> Result<Option<Vec<u8>>>
    where
        A: tilo_actions::Action + std::fmt::Debug + ?Sized,
    {
        self.doc.set_actor(self.actor.clone());
        // A clean commit boundary and the baseline to compare against:
        // `get_heads` closes any open transaction, so what follows belongs
        // to this action alone.
        let before = self.doc.get_heads();
        let text_before = block_texts(&self.doc);
        action
            .apply(&mut self.doc)
            .map_err(|e| anyhow::anyhow!("apply {action:?}: {e:?}"))?;
        self.stamp_authorship(&text_before);
        // Stamp what is still open. `AutoCommit` may have already closed
        // the transaction to answer a read between the action's writes; in
        // that case the change went out undated (`time_ms == 0`), and there
        // is no fixing it here — the writer not closing its own
        // transaction is the app's call, not this one's, so it is said out
        // loud rather than left to look like a rendering fault elsewhere.
        if self.doc.pending_ops() > 0 {
            tilo_resource::history::commit_stamped_with_message(
                &mut self.doc,
                self.history_context
                    .as_ref()
                    .and_then(tilo_actions::ActionContext::change_message),
            );
        } else if self.doc.get_heads() != before {
            tracing::warn!(
                action = ?action,
                "this action closed its own transaction before the commit \
                 could be stamped, so its change carries no wall clock"
            );
        }
        if self.doc.get_heads() == before {
            return Ok(None);
        }
        let delta = self.doc.save_incremental();
        Ok((!delta.is_empty()).then_some(delta))
    }

    /// Apply an action belonging to some app other than the one this
    /// replica's document schema is — the agent answering in a channel —
    /// under the agent's own actor.
    ///
    /// No `qdoc` history stamp: that stamp writes one app's undo/authorship
    /// schema, and putting it on another app's document would be this
    /// agent inventing structure it does not own. The actor is still the
    /// agent's, so the message is attributed to it exactly as its edits
    /// are.
    ///
    /// # Errors
    ///
    /// When `action.apply` fails against this document.
    pub fn apply_foreign<A: tilo_actions::Action>(
        &mut self,
        action: &A,
    ) -> Result<Option<Vec<u8>>> {
        self.doc.set_actor(self.actor.clone());
        action
            .apply(&mut self.doc)
            .map_err(|e| anyhow::anyhow!("apply a foreign action: {e:?}"))?;
        // No `pending_ops` short-circuit: an app's own writer often reads
        // as it goes, closing the transaction before control returns here,
        // so what actually settles it is whether there are unsaved changes.
        self.doc.commit();
        let delta = self.doc.save_incremental();
        Ok((!delta.is_empty()).then_some(delta))
    }

    /// Perform the operation `address` names, attributed to the agent and
    /// recorded within the task context set by
    /// [`set_history_context`](Self::set_history_context).
    ///
    /// Resolved against a tree built from the document **as it stands right
    /// now**, not the survey it was chosen from: a section that has been
    /// deleted, a file renamed, an operation the app stopped publishing —
    /// each is [`DoorError::Unknown`] here, never dispatched against
    /// whatever now happens to carry that id.
    ///
    /// `registry` is the apps' own decoders: the door decodes the encoded
    /// action through it before running anything, so a type no app
    /// registered is refused rather than applied on trust.
    ///
    /// # Errors
    ///
    /// Any [`DoorError`] `door::invoke` returns — most often
    /// [`DoorError::Unknown`] when `address` no longer names a live
    /// operation in the tree built from `view`.
    pub fn invoke(
        &mut self,
        view: &View,
        address: &ActionAddress,
        argument: Option<&str>,
        registry: &ActionRegistry,
    ) -> Result<Option<Vec<u8>>, DoorError> {
        self.through_door(view, |doc, resources, provenance, transport| {
            door::invoke(
                doc, resources, address, argument, provenance, registry, transport,
            )
        })
    }

    /// Typed counterpart to [`invoke`](Self::invoke). This is the canonical
    /// headless path: the descriptor's complete parameter schema is bound by
    /// the shared door before an app action is decoded or mutates state.
    ///
    /// # Errors
    ///
    /// Returns the live door's [`DoorError`] when the address is stale, the
    /// typed arguments do not satisfy the published schema, or decoding or
    /// execution fails.
    pub fn invoke_json(
        &mut self,
        view: &View,
        address: &ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
        registry: &ActionRegistry,
    ) -> Result<Option<Vec<u8>>, DoorError> {
        self.through_door(view, |doc, resources, provenance, transport| {
            door::invoke_json(
                doc, resources, address, arguments, provenance, registry, transport,
            )
        })
    }

    /// Invoke typed arguments after the caller has verified the live
    /// descriptor's confirmation policy. Keeping this separate from
    /// [`invoke_json`](Self::invoke_json) makes an unconfirmed call fail closed
    /// in the shared door even if a caller forgets its own policy check.
    ///
    /// # Errors
    ///
    /// Returns the live door's [`DoorError`] when the address or arguments no
    /// longer match the published action, or decoding or execution fails.
    pub fn invoke_json_confirmed(
        &mut self,
        view: &View,
        address: &ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
        registry: &ActionRegistry,
    ) -> Result<Option<Vec<u8>>, DoorError> {
        self.through_door(view, |doc, resources, provenance, transport| {
            door::invoke_json_confirmed(
                doc, resources, address, arguments, provenance, registry, transport,
            )
        })
    }

    /// Run one door call against a tree built from `view` now, as the agent
    /// and within the current task context, then stamp authorship and return
    /// the delta it produced.
    fn through_door(
        &mut self,
        view: &View,
        call: impl FnOnce(
            &mut AutoCommit,
            &[Resource],
            InvocationProvenance<'_>,
            &mut LocalTransport,
        ) -> Result<tilo_actions::Dispatched, DoorError>,
    ) -> Result<Option<Vec<u8>>, DoorError> {
        let resources = self.resources(view);
        let mut transport = LocalTransport::new();
        self.doc.set_actor(self.actor.clone());
        let text_before = block_texts(&self.doc);
        let provenance = InvocationProvenance {
            actor: self.actor.to_bytes(),
            ambient: self.history_context.as_ref(),
        };
        call(&mut self.doc, &resources, provenance, &mut transport)?;
        self.stamp_authorship(&text_before);
        self.commit_task_context();
        let delta = self.doc.save_incremental();
        Ok((!delta.is_empty()).then_some(delta))
    }

    fn commit_task_context(&mut self) {
        if self.doc.pending_ops() == 0 {
            return;
        }
        tilo_resource::history::commit_stamped_with_message(
            &mut self.doc,
            self.history_context
                .as_ref()
                .and_then(tilo_actions::ActionContext::change_message),
        );
    }

    /// Mark whatever text this replica just added with the durable
    /// `author` mark, so the document itself records who wrote it. Read
    /// back by [`tilo_doc::author_runs`]. A mark rather than a block field
    /// because authorship follows the characters: splitting a paragraph,
    /// merging two, typing into somebody else's sentence all leave each
    /// run attributed to whoever actually wrote it.
    ///
    /// Only text that *grew* is stamped — a deletion authors nothing, and
    /// rewriting a block's attributes without touching its text makes
    /// nobody the author of the sentence left alone.
    fn stamp_authorship(&mut self, before: &std::collections::BTreeMap<String, String>) {
        if self.author.is_empty() {
            return;
        }
        let after = block_texts(&self.doc);
        let mut spans: Vec<(String, usize, usize)> = Vec::new();
        for (id, text) in &after {
            let was = before.get(id).map_or("", String::as_str);
            if let Some((start, end)) = added_range(was, text) {
                spans.push((id.clone(), start, end));
            }
        }
        if spans.is_empty() {
            return;
        }
        let action = word_processor::actions::RichTextAction::SetMark {
            region: word_processor::actions::RegionKind::Body,
            spans: spans
                .into_iter()
                .map(|(block, start, end)| word_processor::actions::Span { block, start, end })
                .collect(),
            name: tilo_doc::schema::MARK_AUTHOR.to_string(),
            value: Some(word_processor::actions::MarkVal::Str(self.author.clone())),
            also_clear: Vec::new(),
        };
        let _ = action.apply_under(&mut self.doc, &automerge::ROOT);
    }

    /// How many ancestor changes this replica is waiting for.
    ///
    /// Automerge queues a change whose dependencies it has not got, and
    /// nothing about reading the document afterwards says so: it
    /// materialises as the page *before* the gap. So a replica missing one
    /// change in the middle reads exactly like a document with nothing
    /// more in it — this is the one honest way to tell those apart.
    pub fn missing_ancestors(&mut self) -> usize {
        self.doc.get_missing_deps(&[]).len()
    }

    /// The whole document, for a peer that just arrived.
    pub fn full(&mut self) -> Vec<u8> {
        // `save` resets the incremental baseline too; nothing of ours is
        // pending at this point, so that is harmless.
        self.doc.save()
    }

    /// The document as blocks in order.
    ///
    /// # Errors
    ///
    /// When the document held has no qdoc schema — one this editor was not
    /// seeded from a peer's snapshot for, or one from an app that does not
    /// use it.
    pub fn snapshot(&self) -> Result<tilo_doc::DocSnapshot> {
        let c = tilo_doc::model::open(&self.doc, &automerge::ROOT)
            .map_err(|e| anyhow::anyhow!("the document has no qdoc schema: {e:?}"))?;
        Ok(tilo_doc::model::materialize(&self.doc, &c))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::{ROOT, ReadDoc, ScalarValue};
    use tilo_actions::{Action, ActionError, Doc, ExecutionLocus};

    /// An action that asks the document a question between its writes,
    /// which closes the transaction early — the condition a `pending_ops`
    /// guard gets wrong.
    #[derive(Debug)]
    struct ReadsAfterWriting;

    impl Action for ReadsAfterWriting {
        fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
            use automerge::transaction::Transactable;
            doc.put(&ROOT, "note", ScalarValue::Str("written".into()))?;
            let _ = doc.get_heads();
            Ok(())
        }
        fn locus(&self) -> ExecutionLocus {
            ExecutionLocus::ClientLocal
        }
    }

    /// An action that genuinely does nothing — an unknown block id, say.
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

    /// Another app's document in the same room, and one later change to it:
    /// a different schema under a different actor, nothing this replica has
    /// ever seen. The delta names the first change as an ancestor, so a
    /// replica that never had it cannot apply this one.
    fn a_delta_from_another_apps_document() -> Vec<u8> {
        use automerge::ObjType;
        let mut foreign = AutoCommit::new();
        foreign.set_actor(ActorId::from("bob".as_bytes()));
        let buffer = foreign
            .put_object(&ROOT, "files.rename_query", ObjType::Text)
            .expect("seed the other app's document");
        foreign.commit();
        let _ = foreign.save_incremental();
        foreign
            .splice_text(&buffer, 0, 0, "launch.md")
            .expect("type into it");
        foreign.commit();
        foreign.save_incremental()
    }

    /// Whether `apply` and `merge_incremental` say a change happened is the
    /// broadcast decision, so each must be honest in both directions.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each case keeps its original fixture and assertions"
    )]
    fn the_editor_reports_a_change_to_send_exactly_when_the_document_changed() {
        // Case `a_delta_from_another_document_is_refused_and_leaves_nothing_queued`.
        // A delta belonging to another app's document is refused, and leaves
        // nothing behind. The agent joins with an empty app id, so the broker
        // hands it every application's frames and this delta is tried against
        // the page as a matter of course.
        {
            let delta = a_delta_from_another_apps_document();

            {
                let mut probe = AutoCommit::new();
                assert!(
                    probe.load_incoming(&delta).is_ok(),
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
                "refusing a foreign delta must leave nothing queued behind it"
            );
        }

        // Case `an_edit_the_agent_makes_carries_the_time_it_was_made`.
        // An edit the agent makes carries the time it was made — a version has
        // a wall clock only because [`Editor::apply`] routes the commit
        // through `commit_stamped`.
        {
            #[derive(Debug)]
            struct JustWrites;
            impl Action for JustWrites {
                fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
                    doc.put(&ROOT, "note", ScalarValue::Str("typed".into()))?;
                    Ok(())
                }
                fn locus(&self) -> ExecutionLocus {
                    ExecutionLocus::ClientLocal
                }
            }

            let mut ed = Editor::empty("Scribe");
            ed.apply(&JustWrites)
                .expect("apply")
                .expect("a plain write is a change to send");

            let versions = tilo_resource::history::versions(&mut ed.doc);
            assert!(!versions.is_empty(), "the edit produced no version to date");
            assert!(
                versions.iter().all(|v| v.time_ms > 0),
                "every change the agent writes must carry the time it was \
                 made: {versions:?}"
            );
        }

        // Case `a_change_is_sent_even_when_the_action_read_after_writing`.
        // A change whose action read after writing is still broadcast. The
        // probe is checked first, so this proves the condition it claims to
        // exercise rather than passing against any guard at all.
        {
            let mut probe = AutoCommit::new();
            ReadsAfterWriting
                .apply(&mut probe)
                .expect("the probe applies");
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

            let mut peer = AutoCommit::new();
            peer.load_incoming(&delta).expect("the delta applies");
            assert_eq!(
                peer.get(&ROOT, "note")
                    .unwrap()
                    .and_then(|(v, _)| v.as_str().map(str::to_string)),
                Some("written".to_string()),
                "the peer never received the change the agent made"
            );
        }

        // Case `an_action_that_changes_nothing_sends_nothing`.
        // And the silence is still honest: an action that changes nothing
        // broadcasts nothing.
        {
            let mut ed = Editor::empty("Scribe");
            assert_eq!(ed.apply(&Nothing).expect("apply"), None);
        }

        // Case `an_ordinary_write_is_still_committed_and_sent`.
        // The ordinary path is unchanged: an action that leaves its ops
        // pending is committed, stamped, and sent.
        {
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
            peer.load_incoming(&delta).expect("the delta applies");
            assert_eq!(
                peer.get(&ROOT, "note")
                    .unwrap()
                    .and_then(|(v, _)| v.as_str().map(str::to_string)),
                Some("typed".to_string())
            );
        }
    }

    #[test]
    fn comment_actions_are_discovered_and_applied_from_the_live_resource_tree() {
        let mut document = AutoCommit::new();
        let containers = tilo_doc::model::init(&mut document, &ROOT);
        let mut ids = tilo_doc::model::seeded_id_gen(&document, &containers);
        let block = {
            let mut dm = tilo_doc::DocMut {
                doc: &mut document,
                c: &containers,
                ids: &mut ids,
            };
            let block = dm.insert_block(None, "paragraph");
            dm.splice_text(&block, 0, 0, "Review this sentence.");
            block
        };
        document.commit();

        let mut editor = Editor::from_snapshot(&document.save(), "Scribe")
            .expect("the seeded document is a word processor document");
        let view = View {
            focus_block: Some(block.clone()),
            target_blocks: vec![block.clone()],
            selection: Some(word_processor::actions::SelectionTarget {
                start_block: block.clone(),
                start: 0,
                end_block: block,
                end: 6,
            }),
            ..View::default()
        };
        let registry = crate::surface::registry();
        let add = tilo_actions::door::ActionAddress::new("word-processor.document", "add-comment");
        let add_args = serde_json::json!({
            "author": "Scribe",
            "date": "2026-09-08",
            "text": "Please clarify this."
        })
        .as_object()
        .cloned()
        .expect("object arguments");

        assert!(
            editor
                .invoke_json(&view, &add, &add_args, &registry)
                .expect("the live document exports add-comment")
                .is_some(),
            "adding a comment changes the shared document"
        );
        let snap = editor.snapshot().expect("materialize the comment");
        let comment = snap.comments.first().expect("the comment was stored");
        assert_eq!(comment.author, "Scribe");
        assert_eq!(comment.blocks[0].text, "Please clarify this.");

        let reply = tilo_actions::door::ActionAddress::new(
            format!("word-processor.comment:{}", comment.id),
            "reply",
        );
        let reply_args = serde_json::json!({
            "author": "Scribe",
            "date": "2026-09-08",
            "text": "Clarified."
        })
        .as_object()
        .cloned()
        .expect("object arguments");
        assert!(
            editor
                .invoke_json(&view, &reply, &reply_args, &registry)
                .expect("the materialized comment exports reply")
                .is_some(),
            "replying changes the shared document"
        );
        let snap = editor.snapshot().expect("materialize the reply");
        assert_eq!(snap.comments.len(), 1);
        assert_eq!(snap.comments[0].replies.len(), 1);
        assert_eq!(snap.comments[0].replies[0].author, "Scribe");
        assert_eq!(snap.comments[0].replies[0].blocks[0].text, "Clarified.");
    }

    /// A destructive operation the agent performs after its policy gate
    /// confirmed it lands in history within the run that asked for it, not
    /// only under the resource it touched.
    #[test]
    fn a_confirmed_destructive_invocation_is_recorded_within_the_run() {
        let mut document = AutoCommit::new();
        let containers = tilo_doc::model::init(&mut document, &ROOT);
        let mut ids = tilo_doc::model::seeded_id_gen(&document, &containers);
        let (paragraph, image) = {
            let mut dm = tilo_doc::DocMut {
                doc: &mut document,
                c: &containers,
                ids: &mut ids,
            };
            let paragraph = dm.insert_block(None, "paragraph");
            dm.splice_text(&paragraph, 0, 0, "Draft figure below.");
            let image = dm.insert_block(Some(&paragraph), "image");
            (paragraph, image)
        };
        document.commit();

        let mut editor = Editor::from_snapshot(&document.save(), "Scribe")
            .expect("the seeded document is a word processor document");
        editor.set_history_context(Some(
            tilo_actions::ActionContext::for_resource(
                "Agent task",
                "active document",
                "document",
                "active document",
            )
            .with_app("org.tilo.Agent", "Agent")
            .with_purpose("Remove the draft figure")
            .with_group("agent-run:ins-1", "Remove the draft figure", "agent-run"),
        ));
        let view = View {
            focus_block: Some(paragraph),
            ..View::default()
        };
        let registry = crate::surface::registry();
        let delete = ActionAddress::new(format!("word-processor.image:{image}"), "delete");
        let arguments = serde_json::Map::new();

        assert_eq!(
            editor
                .invoke_json(&view, &delete, &arguments, &registry)
                .unwrap_err(),
            DoorError::ConfirmationRequired,
            "deleting an image is destructive"
        );
        assert!(
            editor
                .invoke_json_confirmed(&view, &delete, &arguments, &registry)
                .expect("the confirmed deletion runs")
                .is_some(),
            "the deletion changes the shared document"
        );

        let heads = editor.doc.get_heads();
        let change = editor
            .doc
            .get_change_by_hash(&heads[0])
            .expect("the deletion was committed");
        assert_eq!(change.actor_id(), &editor.actor);
        let context = change
            .message()
            .and_then(tilo_actions::ActionContext::from_change_message)
            .expect("the deletion carries its provenance");
        assert_eq!(context.action_label, "Delete image");
        assert_eq!(
            context
                .resource
                .as_ref()
                .map(|resource| resource.id.clone()),
            Some(format!("word-processor.image:{image}"))
        );
        let group = context.group.expect("the run's group is recorded");
        assert_eq!(group.id, "agent-run:ins-1");
        assert_eq!(group.kind, "agent-run");
        assert_eq!(context.purpose.as_deref(), Some("Remove the draft figure"));
    }
}
