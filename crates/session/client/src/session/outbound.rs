//! What this session writes: its ownership of a device's writes.
//!
//! A write a page publishes is drafted, sealed and signed by this device
//! ([`tilo_peer_state::workspace_documents::DeviceWrites::write`]) once its
//! record's keys are here ([`Task::commit_held`]), then published as it was
//! signed ([`Task::publish_request`]); never in the clear, so writes wait for
//! the keys. The replica's ownership is what this session owes its
//! host: it is sent as commits ([`Task::send_ownership`]) and retired as the
//! its peer is sent them, and a barrier completes once
//! everything held when it was asked is answered.

use super::{
    BTreeMap, BTreeSet, CoeditPayload, Completion, DocumentRoute, Envelope, FailureKind,
    OwnershipEntry, ServerClient, Task,
};
use tilo_peer_state::owner::WriteRefusal;
use tilo_server_protocol::WorkspaceId;

use crate::network::{Kept, Waits};

/// A record's history is full and this device could not compact it: its writes wait.
const RECORD_FULL: &str = "record-history-full";
/// This device's signer did not sign its writes: they wait.
const SIGNING_FAILED: &str = "signing-failed";
/// The failures of writes that wait, held: gone once nothing is held.
const HELD_CODES: &[&str] = &[RECORD_FULL, SIGNING_FAILED];

/// What a barrier answers for: every write of this session, the writes to some workspaces alone
/// ([`crate::Outbox::barrier_of`]), or the writes to some documents alone
/// ([`crate::Outbox::barrier_for`]).
#[derive(Debug)]
pub(super) enum Scope {
    /// Every workspace this session joined.
    Session,
    /// These workspaces alone.
    Workspaces(BTreeSet<WorkspaceId>),
    /// These documents alone, whatever else their workspaces hold back.
    Documents(BTreeSet<DocumentRoute>),
}

impl Scope {
    /// Whether it answers for the writes to `route`.
    fn covers(&self, route: &DocumentRoute) -> bool {
        match self {
            Self::Session => true,
            Self::Workspaces(workspaces) => within(Some(workspaces), route),
            Self::Documents(documents) => documents.contains(route),
        }
    }

    /// Whether it answers for writes to `workspace`.
    fn reaches(&self, workspace: WorkspaceId) -> bool {
        match self {
            Self::Session => true,
            Self::Workspaces(workspaces) => workspaces.contains(&workspace),
            Self::Documents(documents) => documents
                .iter()
                .any(|route| route.workspace_id().is_ok_and(|named| named == workspace)),
        }
    }
}

/// The document a write in this device's ownership was written to, where its bytes say.
fn written_to(entry: &OwnershipEntry) -> Option<DocumentRoute> {
    DocumentRoute::of_message(&postcard::from_bytes(&entry.1).ok()?)
}

/// A barrier waiting on what this device owes: the writes still unanswered of those it was
/// asked about, the refusal one of them met, where its outcome goes, what it answers for, and
/// whose refusals it hears.
pub(super) type Owed = (BTreeSet<[u8; 32]>, Option<String>, Completion, Scope, Hears);

/// A barrier called while writes were held: the routes they are held on, where its outcome goes,
/// what it answers for, and whose refusals it hears.
pub(super) type Held = (BTreeSet<DocumentRoute>, Completion, Scope, Hears);

/// The writes whose refusal a barrier answers with ([`crate::Outbox::barrier_for`]): those to
/// these routes alone, the writes of whoever asked, or, with none named, every write it answers
/// for. A write another caller made to the same document keeps it waiting until it is answered,
/// and its refusal is that caller's, not this barrier's.
pub(super) type Hears = Option<BTreeSet<DocumentRoute>>;

/// Whether a barrier that hears `hears` answers with the refusal of a write to `route`.
fn hears(hears: Option<&BTreeSet<DocumentRoute>>, route: &DocumentRoute) -> bool {
    hears.is_none_or(|routes| routes.contains(route))
}

/// Whether `route` is of a workspace `scope` answers for.
fn within(scope: Option<&BTreeSet<WorkspaceId>>, route: &DocumentRoute) -> bool {
    scope.is_none_or(|workspaces| {
        route
            .workspace_id()
            .is_ok_and(|workspace| workspaces.contains(&workspace))
    })
}

/// What became of a write this session handed to its device's ownership
/// ([`Task::publish_request`]).
enum Publishing {
    /// Ownership carries it, and its host answers for it.
    Owned,
    /// Its record is not here yet, or this device has not signed it: it is kept, and goes when
    /// what it waits for comes.
    Waiting,
    /// Its record's history is full: it is kept as a signed write, and goes once the record is
    /// compacted.
    History,
    /// The workspace refused it for good: it is never saved.
    Refused(WriteRefusal),
}

impl Task {
    /// `route` holds new writes of this device, held as the session was told of them: to commit
    /// to its replica sealed with the route's keys ([`Self::commit_held`]). Never in the clear:
    /// the writes wait for this device's keys.
    pub(super) fn publish(&mut self, route: &DocumentRoute) {
        if route.workspace_id().is_err() {
            self.held.lock().remove(route);
            self.fail(
                "protocol-rejected",
                "A document names an invalid workspace.",
            );
            return;
        }
        self.held_since
            .entry(route.clone())
            .or_insert_with(tilo_clock::now);
        self.open_channel(route);
        if self.keys_of(route).is_some() {
            self.outbound_due = true;
        } else {
            self.persist_keys(true);
        }
    }

    /// Draft and publish the writes held for their route's keys, once they may: every held write
    /// of a route whose keys this device holds, taken in turn. Writes the record cannot take yet
    /// stay held, durably, for the next time this device writes; only writes the workspace refuses
    /// for good are let go, and a barrier waiting on them completes with the refusal: a write let
    /// go is never answered as saved.
    pub(super) async fn commit_held(&mut self) {
        self.commit_signed_waiting();
        if !std::mem::take(&mut self.outbound_due) {
            return;
        }
        let ready: Vec<DocumentRoute> = self
            .held
            .lock()
            .keys()
            .filter(|route| self.keys_of(route).is_some())
            .cloned()
            .collect();
        let mut refused = BTreeMap::new();
        // Whether what is held changed: what did not change is not kept again.
        let mut drained = false;
        for route in ready {
            loop {
                let Some(mutation) = self
                    .held
                    .lock()
                    .get(&route)
                    .and_then(|writes| writes.first())
                    .cloned()
                else {
                    break;
                };
                match self.commit_held_write(&route, &mutation).await {
                    // Still waiting for its record or its keys: it stays first in line.
                    None => break,
                    Some(Ok(())) => {
                        self.drop_held(&route, 1);
                        drained = true;
                    }
                    // Refused for good: what was written after it goes with it.
                    Some(Err(reason)) => {
                        refused.insert(route.clone(), reason);
                        let held = self.held.lock().remove(&route);
                        if held.is_some_and(|held| !held.is_empty()) {
                            drained = true;
                        }
                        break;
                    }
                }
            }
        }
        if drained && self.held.lock().is_empty() {
            self.reports.change(|status| {
                status.failures.retain(|failure| {
                    failure.kind != FailureKind::Session || !HELD_CODES.contains(&failure.code)
                });
            });
        }
        {
            // What holds nothing any more waits for nothing.
            let held = self.held.lock();
            self.held_since.retain(|route, _| held.contains_key(route));
            self.kept_why.retain(|route, _| held.contains_key(route));
        }
        self.persist_keys(drained);
        self.release_barriers(&refused);
    }

    /// Take `count` writes of `route` out of what this session holds of it, and let the route go
    /// once nothing waits on it.
    fn drop_held(&self, route: &DocumentRoute, count: usize) {
        let mut held = self.held.lock();
        if let Some(writes) = held.get_mut(route) {
            writes.drain(..count.min(writes.len()));
            if writes.is_empty() {
                held.remove(route);
            }
        }
    }

    /// Publish `op`, a write of this device that its holder signed and this replica admitted
    /// already, as its outgoing ownership: ownership carries it as it is and its host answers
    /// for it, as for any write of this device's. A publication that has to wait (its record is
    /// not here yet, or this device has not signed it) is kept and tried again.
    pub(super) fn publish_signed(&mut self, route: &DocumentRoute, op: collab_types::SignedOp) {
        let Some(workspace) = tilo_desktop_model::WorkspaceId::parse(&route.cluster_id) else {
            self.fail(
                "protocol-rejected",
                "A document names an invalid workspace.",
            );
            return;
        };
        match self.publish_request(
            route,
            workspace,
            route.app_id.clone(),
            route.document.clone(),
            None,
            op,
        ) {
            Publishing::Refused(refusal) => {
                let refused = BTreeMap::from([(route.clone(), refusal.to_string())]);
                self.note_refused(route, &refusal);
                self.release_barriers(&refused);
            }
            Publishing::History => self.note_history(route),
            Publishing::Owned | Publishing::Waiting => {}
        }
    }

    /// Commit the signed writes still waiting for what they need, in the turn.
    pub(super) fn commit_signed_waiting(&mut self) {
        if self.pending_signed.is_empty() {
            return;
        }
        let waiting = std::mem::take(&mut self.pending_signed);
        let mut refused = BTreeMap::new();
        let mut changed = false;
        for (route, requests) in waiting {
            let Some(workspace) = tilo_desktop_model::WorkspaceId::parse(&route.cluster_id) else {
                continue;
            };
            for envelope in requests {
                match self.publish_request(
                    &route,
                    workspace,
                    route.app_id.clone(),
                    route.document.clone(),
                    None,
                    envelope,
                ) {
                    // Refused for good: what was written after it goes with it.
                    Publishing::Refused(refusal) => {
                        self.note_refused(&route, &refusal);
                        refused.insert(route.clone(), refusal.to_string());
                        changed = true;
                        break;
                    }
                    // Its record's history is full: kept as a signed write, and tried again as
                    // the turn goes on.
                    Publishing::History => self.note_history(&route),
                    // What it waited for came: ownership carries it now.
                    Publishing::Owned => changed = true,
                    // Still waiting for its record or its signature.
                    Publishing::Waiting => {}
                }
            }
        }
        if changed {
            // A barrier this route kept waiting goes on to what ownership now carries, or hears
            // the refusal; one still waiting on it keeps waiting.
            self.release_barriers(&refused);
        }
    }

    /// Take this device's signed write into its workspace: admitted where it is not held yet,
    /// kept as a frame a peer is sent, and durable here.
    fn publish_request(
        &mut self,
        route: &DocumentRoute,
        workspace: tilo_desktop_model::WorkspaceId,
        app: String,
        document: String,
        readers: Option<String>,
        envelope: collab_types::SignedOp,
    ) -> Publishing {
        let now = crate::clock::system_time(tilo_clock::unix_now_ns());
        let retry = envelope.clone();
        match self.replicas.owner().admit_writes(
            workspace,
            vec![tilo_peer_state::owner::WorkspaceDocumentWrite {
                app,
                document,
                readers,
                envelope,
            }],
            now,
        ) {
            Ok(()) => {
                self.owes(route);
                self.landed(route, Some(self.config.cert.device_public_key));
                self.count_ownership();
                Publishing::Owned
            }
            Err(refusals @ (WriteRefusal::Waiting(_) | WriteRefusal::Unsigned(_))) => {
                tracing::debug!(app = %route.app_id, record = %route.document, %refusals,
                    "a signed write waits to be published");
                self.pending_signed
                    .entry(route.clone())
                    .or_default()
                    .push(retry);
                Publishing::Waiting
            }
            Err(refusal @ WriteRefusal::HistoryFull { .. }) => {
                tracing::debug!(app = %route.app_id, record = %route.document, %refusal,
                    "a signed write waits for its record's history to be compacted");
                self.pending_signed
                    .entry(route.clone())
                    .or_default()
                    .push(retry);
                Publishing::History
            }
            Err(refusal) => {
                tracing::warn!(app = %route.app_id, record = %route.document, %refusal,
                    "the workspace refused a write of this device for good");
                Publishing::Refused(refusal)
            }
        }
    }

    /// Draft, seal, sign and admit `mutation` of `route` through this device's holder, then
    /// publish the signed op it answered ([`Self::publish_request`]). Answers `None` while the
    /// write waits for its record's keys, so it stays held; a write published waits in this
    /// device's ownership, or as a signed write for its record or a signature; a write the
    /// workspace refuses for good is answered with why, and is never kept.
    ///
    /// A change needs the record it changes. The keys of a record this device opened can come
    /// before the record does, and a change drafted then would be refused and let go, while the
    /// next change, which builds on it, is taken once the record is here: the record would hold a
    /// change whose base no device ever sends, and every reader is refused it as incomplete. The
    /// write waits for the record as it waits for its keys, and goes as it lands
    /// ([`Self::landed`]); a write that founds the record needs none.
    async fn commit_held_write(
        &mut self,
        route: &DocumentRoute,
        mutation: &collab_types::DocumentMutation,
    ) -> Option<Result<(), String>> {
        let keys = self.keys_of(route)?;
        let Some(workspace) = tilo_desktop_model::WorkspaceId::parse(&route.cluster_id) else {
            const INVALID: &str = "A document names an invalid workspace.";
            self.fail("protocol-rejected", INVALID);
            return Some(Err(INVALID.to_owned()));
        };
        let snapshot = matches!(mutation, collab_types::DocumentMutation::Snapshot(_));
        let changes = match mutation {
            collab_types::DocumentMutation::Snapshot(bytes)
            | collab_types::DocumentMutation::Incremental(bytes) => bytes,
        };
        // A founding write claims the workspace holds nothing of the record. A page's first
        // write after a reload is the record whole over one the workspace holds already: a
        // checkpoint, not a founding.
        let founding = snapshot && self.holds_none_of(route);
        let written = self
            .engine
            .document_writes()
            .write(
                &route.cluster_id,
                changes,
                tilo_peer_state::workspace_documents::DocumentWriting {
                    app: &route.app_id,
                    document: &route.document,
                    readers: None,
                    founding,
                    snapshot,
                    sealed: Some(&keys),
                },
            )
            .await;
        match written {
            Ok(written) => {
                match self.publish_request(
                    route,
                    workspace,
                    route.app_id.clone(),
                    route.document.clone(),
                    None,
                    written.envelope,
                ) {
                    Publishing::Owned => Some(Ok(())),
                    // Kept until its record or this device's signature comes: the route stays
                    // one this session waits on, so a barrier asked after it waits too.
                    Publishing::Waiting => Some(Ok(())),
                    // Kept as a signed write until its record is compacted: the route stays one
                    // this session waits on, so a barrier asked after it waits too.
                    Publishing::History => {
                        self.note_history(route);
                        Some(Ok(()))
                    }
                    Publishing::Refused(refusal) => Some(self.refused_write(route, &refusal)),
                }
            }
            Err(WriteRefusal::HistoryFull { .. }) => self.history_full(route),
            Err(WriteRefusal::Unsigned(error)) => {
                tracing::warn!(%error, "writes wait for this device to sign them");
                self.keep_for(route, Waits::Signature);
                self.fail(
                    SIGNING_FAILED,
                    "This device could not sign your latest changes. They are kept on this device.",
                );
                None
            }
            Err(WriteRefusal::Waiting(prerequisite)) => {
                self.keep_for(route, Waits::Prerequisite(prerequisite.to_string()));
                tracing::debug!(app = %route.app_id, record = %route.document, %prerequisite,
                    "writes wait for what the workspace has not admitted here yet");
                None
            }
            Err(
                refusal @ (WriteRefusal::Refused(_)
                | WriteRefusal::Declined { .. }
                | WriteRefusal::ChangeRefused(_)),
            ) => Some(self.refused_write(route, &refusal)),
        }
    }

    /// This device's write to `route` waits for its record's history to be compacted: it stays
    /// held, and whoever waits on the route waits too.
    fn history_full(&mut self, route: &DocumentRoute) -> Option<Result<(), String>> {
        self.keep_for(route, Waits::History);
        self.note_history(route);
        None
    }

    /// Note that this device's write to `route` waits for its record's history to be compacted.
    fn note_history(&self, route: &DocumentRoute) {
        tracing::warn!(app = %route.app_id, record = %route.document,
            "writes wait for their record's history to be compacted");
        self.fail(
            RECORD_FULL,
            "This record is too large to share your latest changes. They are kept on this device.",
        );
    }

    /// Say that the workspace refused this device's write to `route` for good: why, and the
    /// change's own verdict where the workspace's rules refused it. Whoever waits on the route is
    /// answered with the refusal rather than as saved.
    fn refused_write(&self, route: &DocumentRoute, refusal: &WriteRefusal) -> Result<(), String> {
        self.note_refused(route, refusal);
        Err(refusal.to_string())
    }

    /// Note `refusal`, why the workspace refused this device's write to `route` for good: the
    /// session says why, and a change the workspace's rules refused is filed as its author's
    /// activity reads it.
    fn note_refused(&self, route: &DocumentRoute, refusal: &WriteRefusal) {
        if let WriteRefusal::ChangeRefused(refused) = refusal {
            tracing::debug!(app = %route.app_id, document = %route.document, %refused,
                "the workspace refused a write of this device before it went");
            self.reports.emit(&crate::Event::Refused {
                route: route.clone(),
                changes: refused.changes.clone(),
                refusal: refused.refusal.clone(),
            });
        } else {
            tracing::warn!(app = %route.app_id, record = %route.document, %refusal,
                "the workspace refused a write of this device for good");
        }
        self.fail("write-refused", refusal.to_string());
    }

    /// `workspace`'s frames this device wrote and no peer was sent yet: id, bytes, channel and
    /// retention. A frame is sent once; a peer that took a second copy drops it.
    pub(super) fn ownership(
        &self,
        workspace: tilo_desktop_model::WorkspaceId,
    ) -> Vec<OwnershipEntry> {
        self.replicas
            .owner()
            .frames(workspace)
            .into_iter()
            .filter_map(|bytes| {
                let message: tilo_peer_state::NeighbourMsg = postcard::from_bytes(&bytes).ok()?;
                let (channel, retention) = neighbour_wire::retention(&message, false)?;
                Some((*blake3::hash(&bytes).as_bytes(), bytes, channel, retention))
            })
            .collect()
    }

    /// Send `workspace_id`'s writes in this device's ownership its host was not sent on this
    /// connection, as far as the transport takes them; the rest go as the host's answers make
    /// workspace. Returns `false` when the transport took no more.
    pub(super) fn send_ownership(
        &mut self,
        workspace_id: WorkspaceId,
        client: &ServerClient,
    ) -> bool {
        let Some(workspace) = self.replicas.workspace(workspace_id) else {
            return true;
        };
        let author_device_public_key = self.config.cert.device_public_key;
        let sending = self.sending.entry(workspace).or_default();
        let delivered = match sending.drive(
            &self.replicas.owner(),
            workspace,
            |bytes| self.replicas.owner().sent(workspace, bytes),
            |write| {
                let (channel, body) = (write.channel, write.bytes.to_vec());
                let payload = match write.retention {
                    neighbour_wire::Retention::Checkpoint => {
                        CoeditPayload::Checkpoint { channel, body }
                    }
                    neighbour_wire::Retention::Delta => {
                        CoeditPayload::RetainedDelta { channel, body }
                    }
                };
                let mut op = [0_u8; 16];
                op.copy_from_slice(&write.id[..16]);
                client
                    .send(Envelope::Commit {
                        workspace_id,
                        op,
                        author_device_public_key,
                        authored_at_ns: tilo_clock::unix_now_ns(),
                        delta: payload.encode().unwrap_or_default(),
                    })
                    .is_ok()
            },
        ) {
            Ok(delivered) => delivered,
            // The ownership cannot be read, so nothing of it goes: say so rather than leave the
            // writes waiting for an answer nobody was ever asked for.
            Err(error) => {
                tracing::warn!(%workspace, %error, "this device's ownership cannot be read");
                true
            }
        };
        self.count_ownership();
        delivered
    }

    /// Count what this device's ownership still owes its hosts, over every workspace, and follow
    /// the writes that left it unanswered ([`Self::follow_ownership`]).
    pub(super) fn count_ownership(&mut self) {
        let owner = self.replicas.owner();
        let pending = self
            .workspaces
            .iter()
            .filter_map(|workspace_id| self.replicas.workspace(*workspace_id))
            .map(|workspace| owner.frames(workspace).len())
            .sum();
        self.reports
            .change(|status| status.outgoing_pending = pending);
        self.follow_ownership();
    }

    /// Writes leave ownership unanswered when another write carries them: a compaction of their
    /// record folds the ones it covers, and a renewal carries the change of the write it renews.
    /// A barrier owed one of them is owed what ownership holds now, which carries it, and
    /// completes once that is answered; its writes never come back to be answered themselves.
    fn follow_ownership(&mut self) {
        if self.barriers.is_empty() {
            return;
        }
        // Read once for the barriers that answer for every workspace, and for each of those
        // that answer for their own workspaces or documents.
        let every = self
            .barriers
            .iter()
            .any(|(_, _, _, scope, _)| matches!(scope, Scope::Session))
            .then(|| self.owed(&Scope::Session));
        let (waiting, done): (Vec<_>, Vec<_>) = std::mem::take(&mut self.barriers)
            .into_iter()
            .map(|(mut left, refused, complete, scope, heard)| {
                let own = (!matches!(scope, Scope::Session)).then(|| self.owed(&scope));
                if let Some(owed) = own.as_ref().or(every.as_ref())
                    && !left.is_subset(owed)
                {
                    left.clone_from(owed);
                }
                (left, refused, complete, scope, heard)
            })
            .partition(|(left, ..)| !left.is_empty());
        self.barriers = waiting;
        for (_, refused, complete, ..) in done {
            let _ = complete.send(refused.map_or(Ok(()), Err));
        }
    }

    /// Complete `done` once every write said so far that `scope` answers for is durable at this
    /// device's holder: every write of this session, those to some workspaces alone, or those
    /// to some documents alone. What another workspace, or another document, still holds back
    /// or is owed then keeps nothing waiting here: a write that waits, for its document or for
    /// its keys, says nothing about the writes beside it. It answers with the refusal of a write
    /// that `heard` names alone, or of any it answers for when it names none.
    pub(super) fn barrier(&mut self, done: Completion, scope: Scope, heard: Hears) {
        let held: BTreeSet<DocumentRoute> = self
            .waiting_routes()
            .into_iter()
            .filter(|route| scope.covers(route))
            .collect();
        // A write of its own the workspace refused before this barrier was called is one it asks
        // after: it was said before, and it will never be saved.
        let unheard: Vec<DocumentRoute> = self
            .unheard_refusals
            .keys()
            .filter(|route| scope.covers(route) && hears(heard.as_ref(), route))
            .cloned()
            .collect();
        let refused = unheard
            .iter()
            .filter_map(|route| self.unheard_refusals.remove(route))
            .next_back();
        if let Some(refused) = refused {
            let _ = done.send(Err(refused));
        } else if held.is_empty() {
            self.await_ownership(done, None, scope, heard);
        } else {
            self.held_barriers.push((held, done, scope, heard));
        }
    }

    /// The routes this session still holds a write on, one it has not handed to its device's
    /// ownership yet: those an app wrote and this device has not drafted ([`Self::held`]), and
    /// those drafted and signed but waiting for their record, a signature or a compaction
    /// ([`Self::pending_signed`]). A barrier asked after one waits for it.
    fn waiting_routes(&self) -> BTreeSet<DocumentRoute> {
        let mut routes: BTreeSet<DocumentRoute> = self.held.lock().keys().cloned().collect();
        routes.extend(self.pending_signed.keys().cloned());
        routes
    }

    /// Complete `done` once every write now in this device's ownership is answered by its host, or
    /// the write that carries it is ([`Self::follow_ownership`]): every write, or those `scope`
    /// answers for alone.
    pub(super) fn await_ownership(
        &mut self,
        done: Completion,
        refused: Option<String>,
        scope: Scope,
        heard: Hears,
    ) {
        let owed = self.owed(&scope);
        if owed.is_empty() {
            let _ = done.send(refused.map_or(Ok(()), Err));
        } else {
            self.barriers.push((owed, refused, done, scope, heard));
        }
    }

    /// Every write in this device's ownership, sendable now or not, that `scope` answers for:
    /// over every workspace, over some alone, or to some documents alone.
    fn owed(&self, scope: &Scope) -> BTreeSet<[u8; 32]> {
        let owner = self.replicas.owner();
        self.workspaces
            .iter()
            .filter(|workspace_id| scope.reaches(**workspace_id))
            .filter_map(|workspace_id| self.replicas.workspace(*workspace_id))
            .flat_map(|workspace| {
                let owed: Vec<[u8; 32]> = owner
                    .frames(workspace)
                    .into_iter()
                    .filter_map(|bytes| {
                        let message: tilo_peer_state::NeighbourMsg =
                            postcard::from_bytes(&bytes).ok()?;
                        neighbour_wire::retention(&message, false)
                            .map(|(channel, _)| *blake3::hash(&channel).as_bytes())
                    })
                    .collect();
                match scope {
                    Scope::Session | Scope::Workspaces(_) => owed,
                    Scope::Documents(documents) => self.owed_to(workspace, documents, owed),
                }
            })
            .collect()
    }

    /// Those of `owed`, the writes in this device's ownership of `workspace`, that a barrier of
    /// `documents` waits for: all but the ones known to be written to another document. A write
    /// ownership does not show now, or whose document its bytes do not say, stays among them:
    /// the barrier waits for it sooner than answer saved for a write it cannot place.
    fn owed_to(
        &self,
        workspace: tilo_desktop_model::WorkspaceId,
        documents: &BTreeSet<DocumentRoute>,
        mut owed: Vec<[u8; 32]>,
    ) -> Vec<[u8; 32]> {
        if owed.is_empty() {
            return owed;
        }
        for entry in self.ownership(workspace) {
            if written_to(&entry).is_some_and(|route| !documents.contains(&route)) {
                owed.retain(|id| *id != entry.0);
            }
        }
        owed
    }

    /// Whether this device's replica holds nothing of `route`'s document.
    fn holds_none_of(&self, route: &DocumentRoute) -> bool {
        tilo_desktop_model::WorkspaceId::parse(&route.cluster_id).is_some_and(|workspace| {
            self.replicas.owner().with_current(workspace, |view| {
                Ok(view.peer().holds_document(&route.app_id, &route.document))
            }) == Ok(false)
        })
    }

    /// Note what `route`'s held writes wait for, as this device's replica named it.
    fn keep_for(&mut self, route: &DocumentRoute, waits: Waits) {
        self.kept_why.insert(route.clone(), waits);
    }

    /// Note that `route` has committed writes of this device their holder has not answered,
    /// from now unless it had some already.
    fn owes(&mut self, route: &DocumentRoute) {
        self.kept_why.remove(route);
        self.owed_since
            .entry(route.clone())
            .or_insert_with(tilo_clock::now);
    }

    /// The documents of `scope`'s workspaces that hold writes of this device not committed to
    /// its replica yet, each with whether the first of them is a change.
    fn held_in(&self, scope: Option<&BTreeSet<WorkspaceId>>) -> Vec<(DocumentRoute, bool)> {
        self.held
            .lock()
            .iter()
            .filter(|(route, _)| within(scope, route))
            .map(|(route, writes)| {
                let changes = writes.first().is_some_and(|write| {
                    matches!(write, collab_types::DocumentMutation::Incremental(_))
                });
                (route.clone(), changes)
            })
            .collect()
    }

    /// What this session still keeps of what this device wrote to `workspaces`, a document
    /// each, and what its writes wait for ([`crate::Outbox::kept_of`]): the documents whose
    /// writes are held, not committed to this device's replica yet, then those whose committed
    /// writes their holder has not answered. Read as it stands now, so a write is named with
    /// what keeps it now, whatever kept it before.
    pub(super) fn keeps(&mut self, workspaces: &BTreeSet<WorkspaceId>) -> Vec<Kept> {
        let now = tilo_clock::now();
        let scope = Some(workspaces);
        let mut kept: Vec<Kept> = self
            .held_in(scope)
            .into_iter()
            .map(|(route, changes)| {
                let waits = if self.keys_of(&route).is_none() {
                    Waits::Keys
                } else if changes && self.holds_none_of(&route) {
                    Waits::Document
                } else {
                    self.kept_why.get(&route).cloned().unwrap_or(Waits::Commit)
                };
                let age = self
                    .held_since
                    .get(&route)
                    .map_or(std::time::Duration::ZERO, |since| {
                        now.saturating_duration_since(*since)
                    });
                Kept { route, waits, age }
            })
            .collect();
        let owner = self.replicas.owner();
        let owed: BTreeSet<DocumentRoute> = self
            .workspaces
            .iter()
            .filter(|workspace_id| workspaces.contains(*workspace_id))
            .filter_map(|workspace_id| self.replicas.workspace(*workspace_id))
            .flat_map(|workspace| {
                let unanswered: BTreeSet<[u8; 32]> = owner
                    .frames(workspace)
                    .into_iter()
                    .filter_map(|bytes| {
                        let message: tilo_peer_state::NeighbourMsg =
                            postcard::from_bytes(&bytes).ok()?;
                        neighbour_wire::retention(&message, false)
                            .map(|(channel, _)| *blake3::hash(&channel).as_bytes())
                    })
                    .collect();
                self.ownership(workspace)
                    .into_iter()
                    .filter(move |entry| unanswered.contains(&entry.0))
            })
            .filter_map(|entry| written_to(&entry))
            .collect();
        self.owed_since
            .retain(|route, _| !within(scope, route) || owed.contains(route));
        kept.extend(owed.into_iter().map(|route| {
            let age = self
                .owed_since
                .get(&route)
                .map_or(std::time::Duration::ZERO, |since| {
                    now.saturating_duration_since(*since)
                });
            Kept {
                route,
                waits: Waits::Holder,
                age,
            }
        }));
        kept
    }

    /// Renew this device's sealed writes that expired held while its host was away, so
    /// they still go; a barrier owed the original is owed nothing more for it, as the renewal
    /// carries the same change.
    pub(super) async fn renew_ownership(&mut self) {
        // A frame of this device's own is sent as it was signed; nothing renews it.
    }

    /// Move each held barrier whose routes hold nothing any more on to the
    /// outbox, which now carries what those routes held; one waiting on a
    /// route whose writes were `refused` for good, and that hears that route's
    /// refusals, completes with why at once. A refusal no held barrier hears is
    /// kept for the next barrier that hears its route ([`Self::barrier`]): the
    /// write it refused was said before that barrier, and it is never saved.
    pub(super) fn release_barriers(&mut self, refused: &BTreeMap<DocumentRoute, String>) {
        let heard_by = |(routes, _, _, heard): &Held| {
            routes
                .iter()
                .find(|route| refused.contains_key(*route) && hears(heard.as_ref(), route))
                .and_then(|route| refused.get(route))
                .cloned()
        };
        for (route, why) in refused {
            if !self
                .held_barriers
                .iter()
                .any(|(routes, _, _, heard)| routes.contains(route) && hears(heard.as_ref(), route))
            {
                self.unheard_refusals.insert(route.clone(), why.clone());
            }
        }
        // A barrier waiting on a refused route of its own is answered with the refusal at once:
        // whatever else it waits for, it can no longer complete as saved.
        let (heard, waiting): (Vec<_>, Vec<_>) = std::mem::take(&mut self.held_barriers)
            .into_iter()
            .partition(|barrier| heard_by(barrier).is_some());
        self.held_barriers = waiting;
        for barrier in heard {
            if let Some(why) = heard_by(&barrier) {
                let _ = barrier.1.send(Err(why));
            }
        }
        let held = self.waiting_routes();
        let (waiting, released): (Vec<_>, Vec<_>) = std::mem::take(&mut self.held_barriers)
            .into_iter()
            .partition(|(routes, ..)| routes.iter().any(|route| held.contains(route)));
        self.held_barriers = waiting;
        for (_, done, scope, heard) in released {
            self.await_ownership(done, None, scope, heard);
        }
    }
}
