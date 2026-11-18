//! What this session writes: its ownership of a device's writes.
//!
//! A write a page publishes is committed to this device's replica sealed with
//! its record's keys ([`Task::commit_held`]); never in the clear, so frames
//! wait for the keys. The replica's ownership is what this session owes its
//! host: it is sent as commits ([`Task::send_ownership`]) and retired as the
//! host answers them ([`Task::retire_answered`]), and a barrier completes once
//! everything held when it was asked is answered.

use super::{
    Answered, BTreeMap, BTreeSet, CoeditPayload, Completion, DocumentRoute, Envelope, FailureKind,
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

/// A barrier called while frames were held: the routes they are held on, where its outcome goes,
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

impl Task {
    /// `route` holds new writes of this device, held as the session was told of them: to commit
    /// to its replica sealed with the route's keys ([`Self::commit_held`]). Never in the clear:
    /// the writes wait for this device's keys.
    pub(super) fn publish(&mut self, route: &DocumentRoute) {
        if route.workspace_id().is_err() {
            self.engine.held_writes().lock().remove(route);
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

    /// Commit the writes held for their route's keys, once they may: every held write of a route
    /// whose keys this device holds, in one owner turn per route; held, so read, until committed.
    /// Writes the record cannot take yet stay held, durably, for the next time this device
    /// writes; only writes the workspace refuses for good are let go, and a barrier waiting on
    /// them completes with the refusal: a write let go is never answered as saved.
    pub(super) async fn commit_held(&mut self) {
        if !std::mem::take(&mut self.outbound_due) {
            return;
        }
        let ready: Vec<DocumentRoute> = self
            .engine
            .held_writes()
            .lock()
            .keys()
            .filter(|route| self.keys_of(route).is_some())
            .cloned()
            .collect();
        let mut refused = BTreeMap::new();
        for route in ready {
            let Some(frames) = self.engine.held_writes().lock().get(&route).cloned() else {
                continue;
            };
            let taken = frames.len();
            match self.commit_frames(&route, frames).await {
                None => continue,
                Some(Ok(())) => {}
                Some(Err(reason)) => {
                    refused.insert(route.clone(), reason);
                }
            }
            let mut held = self.engine.held_writes().lock();
            if let Some(frames) = held.get_mut(&route) {
                frames.drain(..taken.min(frames.len()));
                if frames.is_empty() {
                    held.remove(&route);
                }
            }
        }
        if self.engine.held_writes().lock().is_empty() {
            self.reports.change(|status| {
                status.failures.retain(|failure| {
                    failure.kind != FailureKind::Session || !HELD_CODES.contains(&failure.code)
                });
            });
        }
        {
            // What holds nothing any more waits for nothing.
            let held = self.engine.held_writes().lock();
            self.held_since.retain(|route, _| held.contains_key(route));
            self.kept_why.retain(|route, _| held.contains_key(route));
        }
        self.persist_keys(true);
        self.release_barriers(&refused);
    }

    /// Seal, sign and commit `frames` of `route` into this device's ownership, in one owner turn.
    /// Answers `None` while they stay held, and once it is done with them, whether they were
    /// committed or refused for good, and why. Writes its record cannot take yet (its keys went
    /// away, its history is full, or the signer did not sign) stay held. What this device writes, its readers read at once, and ownership keeps however long
    /// its host is away.
    /// A change needs the record it changes. The keys of a record this device opened can come
    /// before the record does, and a change drafted then would be refused and let go, while the
    /// next change, which builds on it, is taken once the record is here: the record would
    /// hold a change whose base no device ever sends, and every reader is refused it as
    /// incomplete. The writes wait for the record as they wait for its keys, and go as it
    /// lands ([`Self::landed`]); a write that founds the record needs none.
    fn waits_for_its_document(
        &self,
        workspace: tilo_desktop_model::WorkspaceId,
        route: &DocumentRoute,
        frames: &[Vec<u8>],
    ) -> bool {
        let changes_first = frames.first().is_some_and(|frame| {
            collab_types::ContentRetention::of_frame(frame) == collab_types::ContentRetention::Delta
        });
        changes_first
            && self.replicas.owner().with_current(workspace, |view| {
                Ok(view.peer().holds_document(&route.app_id, &route.document))
            }) == Ok(false)
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one route's frames sealed, signed and committed, read top to bottom, with what a \
                  refusal of them says"
    )]
    pub(super) async fn commit_frames(
        &mut self,
        route: &DocumentRoute,
        frames: Vec<Vec<u8>>,
    ) -> Option<Result<(), String>> {
        let owner = self.replicas.owner();
        let Some(workspace) = tilo_desktop_model::WorkspaceId::parse(&route.cluster_id) else {
            const INVALID: &str = "A document names an invalid workspace.";
            self.fail("protocol-rejected", INVALID);
            return Some(Err(INVALID.to_owned()));
        };
        let keys = self.keys_of(route)?;
        if self.waits_for_its_document(workspace, route, &frames) {
            return None;
        }
        let artifact = route.artifact();
        let author = self.signer.device_public_key();
        let now = crate::clock::system_time(tilo_clock::unix_now_ns());
        let written = (route.app_id.as_str(), route.document.as_str());
        // Drafted in the owner's turn, signed by this device's ownership outside it.
        let draft = |view: &tilo_peer_state::owner::NativeCommittedView| {
            frames
                .iter()
                .zip(0..)
                .map(|(frame, drafted)| {
                    let mut seal = |plaintext: &[u8]| {
                        keys.seal_content(
                            artifact,
                            collab_types::ContentRetention::of_frame(plaintext),
                            plaintext,
                        )
                        .map_err(|error| error.to_string())
                    };
                    let op = view.peer().draft_write(
                        written,
                        frame,
                        Some(&mut seal),
                        author,
                        drafted,
                        now,
                    )?;
                    Ok(tilo_peer_state::owner_commit::OwnerCommitRequest {
                        workspace: route.cluster_id.clone(),
                        operation: tilo_peer_state::owner_commit::CommitOperation::Document {
                            app: route.app_id.clone(),
                            document: route.document.clone(),
                            envelope: Box::new(op),
                            own: view.peer().own_write(written.0, written.1, &author),
                        },
                    })
                })
                .collect()
        };
        let committed = match owner
            .write(workspace, draft, self.signer.as_ref(), now)
            .await
        {
            Ok(committed) => committed,
            Err(refusal @ WriteRefusal::HistoryFull { .. }) => {
                tracing::warn!(%refusal, "writes wait for their record's history to be compacted");
                self.keep_for(route, Waits::History);
                self.fail(
                    RECORD_FULL,
                    "This record is too large to share your latest changes. They are kept on this device.",
                );
                return None;
            }
            Err(WriteRefusal::Unsigned(error)) => {
                tracing::warn!(%error, "writes wait for this device to sign them");
                self.keep_for(route, Waits::Signature);
                self.fail(
                    SIGNING_FAILED,
                    "This device could not sign your latest changes. They are kept on this device.",
                );
                return None;
            }
            Err(WriteRefusal::Waiting(prerequisite)) => {
                self.keep_for(route, Waits::Prerequisite(prerequisite.to_string()));
                tracing::debug!(app = %route.app_id, record = %route.document, %prerequisite,
                    "writes wait for what the workspace has not admitted here yet");
                return None;
            }
            Err(WriteRefusal::Refused(error)) => {
                tracing::warn!(app = %route.app_id, record = %route.document, %error,
                    "the workspace refused a write of this device for good");
                self.fail("write-refused", error.clone());
                return Some(Err(error));
            }
            // The workspace's rules refused the change here, before it went. Its author reads it
            // where a server's refusal of a change is read: the same typed refusal of the same
            // route and changes, and why.
            Err(WriteRefusal::ChangeRefused(refused)) => {
                tracing::debug!(app = %route.app_id, document = %route.document, %refused,
                    "the workspace refused a write of this device before it went");
                self.reports.emit(&crate::Event::Refused {
                    route: route.clone(),
                    changes: refused.changes.clone(),
                    refusal: refused.refusal.clone(),
                });
                self.fail("write-refused", refused.reason.clone());
                return Some(Err(refused.to_string()));
            }
        };
        if committed.is_empty() {
            return Some(Ok(()));
        }
        self.owes(route);
        self.landed(route, Some(self.config.cert.device_public_key));
        let ids: Vec<[u8; 32]> = committed
            .iter()
            .filter_map(|(request, _)| tilo_peer_state::owner::outgoing_bytes(request).ok())
            .map(|bytes| tilo_server_protocol::compaction::entry_id(&bytes))
            .collect();
        // What this device publishes, its own replica holds.
        self.note_delivered(route, &ids);
        self.count_ownership();
        Some(Ok(()))
    }

    /// `workspace`'s writes in this device's ownership: id, bytes, channel and retention.
    pub(super) fn ownership(
        &self,
        workspace: tilo_desktop_model::WorkspaceId,
    ) -> Vec<OwnershipEntry> {
        self.replicas
            .owner()
            .with_outgoing(workspace, tilo_clock::wall(), |_, outgoing| {
                Ok(outgoing
                    .entries()
                    .map(|entry| {
                        (
                            entry.id(),
                            entry.bytes().to_vec(),
                            entry.channel(),
                            entry.retention(),
                        )
                    })
                    .collect())
            })
            .unwrap_or_default()
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
        let delivered = self
            .sending
            .entry(workspace)
            .or_default()
            .drive(
                &self.replicas.owner(),
                workspace,
                |_| Ok(()),
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
            )
            .unwrap_or(true);
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
            .map(|workspace| owner.unadmitted_writes(workspace).unwrap_or_default())
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
            .engine
            .held_writes()
            .lock()
            .keys()
            .filter(|route| scope.covers(route))
            .cloned()
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
                let owed = owner.owed_writes(workspace).unwrap_or_default();
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
        self.engine
            .held_writes()
            .lock()
            .iter()
            .filter(|(route, _)| within(scope, route))
            .map(|(route, frames)| {
                let changes = frames.first().is_some_and(|frame| {
                    collab_types::ContentRetention::of_frame(frame)
                        == collab_types::ContentRetention::Delta
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
                    .owed_writes(workspace)
                    .unwrap_or_default()
                    .into_iter()
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
        let now = crate::clock::system_time(tilo_clock::unix_now_ns());
        let mut renewed = 0usize;
        for workspace_id in self.workspaces.clone() {
            let Some(workspace) = self.replicas.workspace(workspace_id) else {
                continue;
            };
            match self
                .replicas
                .owner()
                .renew_outgoing(workspace, self.signer.as_ref(), now)
                .await
            {
                Ok(count) => renewed = renewed.saturating_add(count),
                Err(error) => tracing::warn!(%error, "expired sealed writes stay unrenewed"),
            }
        }
        if renewed > 0 {
            self.count_ownership();
        }
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
        let held = self.engine.held_writes().lock().clone();
        let (waiting, released): (Vec<_>, Vec<_>) = std::mem::take(&mut self.held_barriers)
            .into_iter()
            .partition(|(routes, ..)| routes.iter().any(|route| held.contains_key(route)));
        self.held_barriers = waiting;
        for (_, done, scope, heard) in released {
            self.await_ownership(done, None, scope, heard);
        }
    }

    /// Take the host's answer to the write it answered as `op`: durable, or refused with the
    /// host's `refusal` (it can never become durable). Answers retire from ownership together
    /// ([`Self::retire_answered`]).
    pub(super) fn acknowledge(
        &mut self,
        op: [u8; 16],
        outcome: Result<u64, tilo_server_protocol::refusal::Refusal>,
    ) {
        let Some((workspace, id)) = self
            .sending
            .iter_mut()
            .find_map(|(workspace, sending)| Some((*workspace, sending.answered(&op)?)))
        else {
            return;
        };
        // A host that was not ready for the write takes it once it says the workspace is ready:
        // the write stays held and goes again then. One the host refuses as not ready
        // after it said it was is refused: the host will not take it on this connection.
        if let Err(refusal) = &outcome
            && refusal.passes_once_ready()
            && self.not_ready.insert(id)
        {
            tracing::debug!(%refusal, "the host was not ready for a write; it goes again once it is");
            let ready = self
                .reports
                .read(|status| status.workspaces.get(&workspace).copied().unwrap_or(false));
            if let Some(sending) = self.sending.get_mut(&workspace) {
                sending.send_once_ready(id);
                if ready {
                    sending.ready();
                }
            }
            return;
        }
        self.not_ready.remove(&id);
        // What the refused write would have brought, as its envelope claims it: read before the
        // entry leaves ownership, so whoever holds the record withdraws exactly those changes.
        let withdrawn = match &outcome {
            Err(reason) => self
                .ownership(workspace)
                .into_iter()
                .find(|entry| entry.0 == id)
                .and_then(|entry| {
                    Some(crate::Event::Refused {
                        route: DocumentRoute::of_message(&postcard::from_bytes(&entry.1).ok()?)?,
                        changes: tilo_peer_state::document_frame::claimed_changes(&entry.1),
                        refusal: reason.clone(),
                    })
                }),
            Ok(_) => None,
        };
        self.answered.push(Answered {
            workspace,
            id,
            outcome,
            withdrawn,
        });
    }

    /// Retire every write the host answered from ownership, in one owner turn per workspace, then
    /// tell whoever waited for them.
    pub(super) fn retire_answered(&mut self) {
        if self.answered.is_empty() {
            return;
        }
        let answered = std::mem::take(&mut self.answered);
        let mut answers: BTreeMap<_, BTreeMap<_, _>> = BTreeMap::new();
        for answer in &answered {
            let host = match answer.outcome {
                Ok(order) => tilo_peer_state::owner::HostAnswer::Durable(order),
                Err(_) => tilo_peer_state::owner::HostAnswer::Refused,
            };
            answers
                .entry(answer.workspace)
                .or_default()
                .insert(answer.id, host);
        }
        for (workspace, answers) in answers {
            if let Err(error) = self.replicas.owner().retire_answered(workspace, &answers) {
                self.fail_on(FailureKind::Storage, "storage-transaction-failed", error);
            }
        }
        self.outbound_due = true;
        for Answered {
            id,
            outcome,
            withdrawn,
            ..
        } in answered
        {
            let route = match &withdrawn {
                Some(crate::Event::Refused { route, .. }) => Some(route.clone()),
                _ => None,
            };
            if let Some(refused) = withdrawn {
                // The refused write left this device's replica: whoever reads it reads it again.
                self.engine.arrivals().landed();
                self.reports.emit(&refused);
            }
            // The host read this commit and would not take it: the session is
            // fine, and saying so keeps the app connected. What the host said is
            // what the person is owed, so it travels with the refusal.
            let refusal = outcome.err().map(|refusal| refusal_text(&refusal));
            let mut heard = false;
            let (waiting, done): (Vec<_>, Vec<_>) = std::mem::take(&mut self.barriers)
                .into_iter()
                .map(|(mut owed, mut refused, complete, scope, hearing)| {
                    // A barrier owed a write of another caller's waits no more for it, and
                    // its refusal is not this barrier's to answer with.
                    if owed.remove(&id)
                        && route
                            .as_ref()
                            .is_none_or(|route| hears(hearing.as_ref(), route))
                    {
                        heard = true;
                        if refused.is_none() {
                            refused.clone_from(&refusal);
                        }
                    }
                    (owed, refused, complete, scope, hearing)
                })
                // One that heard a refusal is answered at once: it can no longer complete as
                // saved, whatever else it waits for.
                .partition(|(owed, refused, ..)| !owed.is_empty() && refused.is_none());
            self.barriers = waiting;
            for (_, refused, complete, ..) in done {
                let _ = complete.send(refused.map_or(Ok(()), Err));
            }
            // Answered before its caller asked after it: the next barrier that hears its route
            // does ([`Self::barrier`]), as one of a write the workspace refused before it was sent.
            if !heard && let (Some(route), Some(refusal)) = (route, refusal) {
                self.unheard_refusals.insert(route, refusal);
            }
        }
        // Counted once the answers took their writes off the barriers: a write that left ownership
        // answered is not one another write carries.
        self.count_ownership();
    }
}

/// What this session reports when the host would not take a change it read.
fn refusal_text(refusal: &tilo_server_protocol::refusal::Refusal) -> String {
    format!("{REFUSAL}: {refusal}")
}

/// The phrasing this session uses for the one answer that is a refusal.
const REFUSAL: &str = "the workspace refused the change";
