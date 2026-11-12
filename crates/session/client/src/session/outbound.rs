//! What this session writes: its custody of a device's writes.
//!
//! A write a page publishes is committed to this device's replica sealed with
//! its record's keys ([`Task::commit_held`]); never in the clear, so frames
//! wait for the keys. The replica's custody is what this session owes its
//! host: it is sent as commits ([`Task::send_custody`]) and retired as the
//! host answers them ([`Task::retire_answered`]), and a barrier completes once
//! everything in custody when it was asked is answered.

use super::{
    Answered, BTreeMap, BTreeSet, CoeditPayload, Completion, CustodyEntry, DocumentRoute, Envelope,
    FailureSubject, RelayClient, Task,
};

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
        self.open_channel(route);
        if self.keys_of(route).is_some() {
            self.outbound_due = true;
        } else {
            self.persist_channels(true);
        }
    }

    /// Commit the writes held for their route's keys, once they may: every held write of a route
    /// whose keys this device holds, in one owner turn per route; held, so read, until committed.
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
        for route in ready {
            let Some(frames) = self.engine.held_writes().lock().get(&route).cloned() else {
                continue;
            };
            let (_, taken) = self.commit_frames(&route, frames).await;
            let mut held = self.engine.held_writes().lock();
            if let Some(frames) = held.get_mut(&route) {
                frames.drain(..taken.min(frames.len()));
                if frames.is_empty() {
                    held.remove(&route);
                }
            }
        }
        self.persist_channels(true);
        self.release_barriers();
    }

    /// Seal, sign and commit `frames` of `route` into this device's custody, in one owner turn;
    /// the entries it committed, as the host names them, and how many of `frames` it is done with
    /// (all of them, committed or not). What this device writes, its readers read at once, and
    /// custody keeps however long its host is away.
    pub(super) async fn commit_frames(
        &mut self,
        route: &DocumentRoute,
        frames: Vec<Vec<u8>>,
    ) -> (Vec<[u8; 32]>, usize) {
        let owner = self.replicas.owner();
        let Some(workspace) = tilo_desktop_model::WorkspaceId::parse(&route.cluster_id) else {
            self.fail(
                "protocol-rejected",
                "A document names an invalid workspace.",
            );
            return (Vec::new(), frames.len());
        };
        let Some(keys) = self.keys_of(route) else {
            self.fail("signature-rejected", "the route's channel keys went away");
            return (Vec::new(), frames.len());
        };
        // The state carrying these keys is written before a write sealed
        // under them leaves ([`ChannelPeer::save_deferred`]): an admission
        // taken in a moment ago survives a page that reloads now.
        self.channels.save_pending();
        let artifact = route.artifact();
        let author = self.signer.device_public_key();
        let now = crate::clock::system_time(tilo_clock::unix_now_ns());
        // Drafted in the owner's turn, signed by this device's custody outside it.
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
                        (&route.app_id, &route.record),
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
                            record: route.record.clone(),
                            envelope: Box::new(op),
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
            Err(error) => {
                self.fail("signature-rejected", error);
                return (Vec::new(), frames.len());
            }
        };
        if committed.is_empty() {
            return (Vec::new(), frames.len());
        }
        self.landed(route, Some(self.config.cert.device_public_key));
        let ids: Vec<[u8; 32]> = committed
            .iter()
            .filter_map(|(request, _)| tilo_peer_state::owner::outgoing_bytes(request).ok())
            .map(|bytes| tilo_relay_protocol::compaction::entry_id(&bytes))
            .collect();
        // What this device publishes, its own replica holds.
        self.note_delivered(route, &ids);
        self.count_custody();
        (ids, frames.len())
    }

    /// `workspace`'s writes in this device's custody: id, bytes, channel and retention.
    pub(super) fn custody(&self, workspace: tilo_desktop_model::WorkspaceId) -> Vec<CustodyEntry> {
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

    /// Send `workspace_id`'s writes in this device's custody its host was not sent on this
    /// connection, as far as the transport takes them; the rest go as the host's answers make
    /// room. Returns `false` when the transport took no more.
    pub(super) fn send_custody(&mut self, workspace_id: u64, client: &RelayClient) -> bool {
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
        self.count_custody();
        delivered
    }

    /// Count what this device's custody still owes its hosts, over every workspace.
    pub(super) fn count_custody(&self) {
        let owner = self.replicas.owner();
        let pending = self
            .workspaces
            .iter()
            .filter_map(|workspace_id| self.replicas.workspace(*workspace_id))
            .map(|workspace| owner.unadmitted_writes(workspace).unwrap_or_default())
            .sum();
        self.reports
            .change(|status| status.outgoing_pending = pending);
    }

    /// Complete `done` once every write now in this device's custody is answered by its host, or
    /// left it renewed ([`Self::release_renewed`]).
    pub(super) fn await_custody(&mut self, done: Completion) {
        let owed = self.owed();
        if owed.is_empty() {
            let _ = done.send(Ok(()));
        } else {
            self.barriers.push((owed, None, done));
        }
    }

    /// Every write in this device's custody, over every workspace, sendable now or not.
    fn owed(&self) -> BTreeSet<[u8; 32]> {
        let owner = self.replicas.owner();
        self.workspaces
            .iter()
            .filter_map(|workspace_id| self.replicas.workspace(*workspace_id))
            .flat_map(|workspace| owner.owed_writes(workspace).unwrap_or_default())
            .collect()
    }

    /// Renew this device's sealed writes that expired in custody while its host was away, so
    /// they still go; a barrier owed the original is owed nothing more for it, as the renewal
    /// carries the same change.
    pub(super) async fn renew_custody(&mut self) {
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
            self.release_renewed();
        }
    }

    /// Complete each barrier whose writes all left custody, the last of them renewed.
    fn release_renewed(&mut self) {
        let owed = self.owed();
        let (waiting, done): (Vec<_>, Vec<_>) = std::mem::take(&mut self.barriers)
            .into_iter()
            .map(|(mut left, refused, complete)| {
                left.retain(|id| owed.contains(id));
                (left, refused, complete)
            })
            .partition(|(left, _, _)| !left.is_empty());
        self.barriers = waiting;
        for (_, refused, complete) in done {
            let _ = complete.send(refused.map_or(Ok(()), Err));
        }
    }

    /// Move each held barrier whose routes hold nothing any more on to the
    /// outbox, which now carries what those routes held.
    pub(super) fn release_barriers(&mut self) {
        let held = self.engine.held_writes().lock().clone();
        let (waiting, released): (Vec<_>, Vec<_>) = std::mem::take(&mut self.held_barriers)
            .into_iter()
            .partition(|(routes, _)| routes.iter().any(|route| held.contains_key(route)));
        self.held_barriers = waiting;
        for (_, done) in released {
            self.await_custody(done);
        }
    }

    /// Take the host's answer to the write it answered as `op`: durable, or refused with the
    /// host's `refusal` (it can never become durable). Answers retire from custody together
    /// ([`Self::retire_answered`]).
    pub(super) fn acknowledge(&mut self, op: [u8; 16], outcome: Result<u64, String>) {
        let Some((workspace, id)) = self
            .sending
            .iter_mut()
            .find_map(|(workspace, sending)| Some((*workspace, sending.answered(&op)?)))
        else {
            return;
        };
        // What the refused write would have brought, as its envelope claims it: read before the
        // entry leaves custody, so whoever holds the record withdraws exactly those changes.
        let withdrawn = match &outcome {
            Err(reason) => self
                .custody(workspace)
                .into_iter()
                .find(|entry| entry.0 == id)
                .and_then(|entry| {
                    Some(crate::Event::Refused {
                        route: DocumentRoute::of_message(&postcard::from_bytes(&entry.1).ok()?)?,
                        changes: tilo_peer_state::document_frame::claimed_changes(&entry.1),
                        reason: reason.clone(),
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

    /// Retire every write the host answered from custody, in one owner turn per workspace, then
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
                self.fail_on(FailureSubject::Storage, "storage-transaction-failed", error);
            }
        }
        self.outbound_due = true;
        self.count_custody();
        for Answered {
            id,
            outcome,
            withdrawn,
            ..
        } in answered
        {
            if let Some(refused) = withdrawn {
                // The refused write left this device's replica: whoever reads it reads it again.
                self.engine.arrivals().landed();
                self.reports.emit(&refused);
            }
            // The host read this commit and would not take it: the session is
            // fine, and saying so keeps the app connected. What the host said is
            // what the person is owed, so it travels with the refusal.
            let refusal = outcome.err().map(|reason| refusal_text(&reason));
            let (waiting, done): (Vec<_>, Vec<_>) = std::mem::take(&mut self.barriers)
                .into_iter()
                .map(|(mut owed, mut refused, complete)| {
                    if owed.remove(&id) && refused.is_none() {
                        refused.clone_from(&refusal);
                    }
                    (owed, refused, complete)
                })
                .partition(|(owed, _, _)| !owed.is_empty());
            self.barriers = waiting;
            for (_, refused, complete) in done {
                let _ = complete.send(refused.map_or(Ok(()), Err));
            }
        }
    }
}

/// What this session reports when the host would not take a change it read.
fn refusal_text(reason: &str) -> String {
    format!("{REFUSAL}: {}", reason.trim())
}

/// The phrasing this session uses for the one answer that is a refusal.
const REFUSAL: &str = "the workspace refused the change";
