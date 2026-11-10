//! What this session writes: its custody of a device's writes.
//!
//! A write a page publishes is committed to this device's replica sealed with
//! its record's keys ([`Task::commit_held`]); never in the clear, so frames
//! wait for the keys. The replica's custody is what this session owes its
//! host: it is sent as commits ([`Task::send_custody`]) and retired as the
//! host answers them ([`Task::retire_answered`]), and a barrier completes once
//! everything in custody when it was asked is answered.

use super::*;

impl Task {
    /// Take `frames`, cleartext document frames of `route`, to commit to this device's replica
    /// sealed with the route's keys ([`Self::commit_held`]), behind what the route holds already.
    /// Never in the clear: the frames wait for this device's keys.
    pub(super) fn publish(&mut self, route: DocumentRoute, frames: Vec<Vec<u8>>) {
        if route.workspace_id().is_err() {
            self.fail(
                "protocol-rejected",
                "A document names an invalid workspace.",
            );
            return;
        }
        self.open_channel(&route);
        let ready = self.ciphers.contains_key(&route);
        self.held_outbound.entry(route).or_default().extend(frames);
        if ready {
            self.outbound_due = true;
        } else {
            self.persist_channels(true);
        }
    }

    /// Commit the writes held for their route's keys or for room in custody, once they may: every
    /// held write of a route whose keys this device holds, in one owner turn per route.
    pub(super) async fn commit_held(&mut self) {
        if !std::mem::take(&mut self.outbound_due) {
            return;
        }
        let ready: Vec<DocumentRoute> = self
            .held_outbound
            .keys()
            .filter(|route| self.ciphers.contains_key(*route))
            .cloned()
            .collect();
        for route in ready {
            if let Some(frames) = self.held_outbound.remove(&route) {
                self.commit_frames(&route, frames, false).await;
            }
        }
        self.persist_channels(true);
        self.release_barriers();
    }

    /// Seal, sign and commit `frames` of `route` into this device's custody, in one owner turn;
    /// the entries it committed, as the host names them. What this device writes, its readers
    /// read at once. What custody has no room for waits with the route's held writes and commits
    /// once the host answered some of what it holds; `whole` commits every frame or none.
    pub(super) async fn commit_frames(
        &mut self,
        route: &DocumentRoute,
        frames: Vec<Vec<u8>>,
        whole: bool,
    ) -> Vec<[u8; 32]> {
        let owner = self.replicas.owner();
        let Some(workspace) = tilo_desktop_model::WorkspaceId::parse(&route.cluster_id) else {
            self.fail(
                "protocol-rejected",
                "A document names an invalid workspace.",
            );
            return Vec::new();
        };
        let before: BTreeSet<[u8; 32]> = self
            .custody(workspace)
            .into_iter()
            .map(|entry| entry.0)
            .collect();
        // Only what custody has room for is sealed and signed now.
        let (entries, bytes) = owner.custody_room(workspace).unwrap_or_default();
        let room = frames
            .iter()
            .scan((entries, bytes), |(entries, bytes), frame| {
                let fits = *entries > 0 && frame.len() < *bytes;
                *entries = entries.saturating_sub(1);
                *bytes = bytes.saturating_sub(frame.len());
                Some(fits)
            })
            .take_while(|fits| *fits)
            .count();
        if whole && room < frames.len() {
            return Vec::new();
        }
        let signer = Arc::clone(&self.signer);
        let mut frames = std::collections::VecDeque::from(frames);
        let mut taken = Vec::new();
        let mut writes = Vec::new();
        while taken.len() < room {
            let Some(frame) = frames.pop_front() else {
                break;
            };
            let Some(cipher) = self.ciphers.get_mut(route) else {
                self.fail("signature-rejected", "the route's channel keys went away");
                return Vec::new();
            };
            match crate::document::signed_write(
                route,
                &frame,
                signer.as_ref(),
                cipher.as_mut(),
                tilo_clock::unix_now_ns(),
            )
            .await
            {
                Ok(write) => {
                    writes.push(write);
                    taken.push(frame);
                }
                Err(error) => {
                    self.fail("signature-rejected", error.to_string());
                    return Vec::new();
                }
            }
        }
        let committed = match owner.commit_documents(
            workspace,
            writes,
            whole,
            crate::clock::system_time(tilo_clock::unix_now_ns()),
        ) {
            Ok(committed) => committed,
            Err(error) => {
                self.fail("signature-rejected", error);
                return Vec::new();
            }
        };
        if !whole {
            let mut rest = taken.split_off(committed.min(taken.len()));
            rest.extend(frames);
            if !rest.is_empty() {
                let held = self.held_outbound.entry(route.clone()).or_default();
                let later = std::mem::take(held);
                held.extend(rest);
                held.extend(later);
                self.persist_channels(true);
            }
        }
        if committed == 0 {
            return Vec::new();
        }
        self.landed(route, Some(self.config.cert.device_public_key));
        let committed: Vec<[u8; 32]> = self
            .custody(workspace)
            .into_iter()
            .filter(|entry| !before.contains(&entry.0))
            .map(|entry| tilo_relay_protocol::compaction::entry_id(&entry.1))
            .collect();
        // What this device publishes, its own replica holds.
        self.note_delivered(route, &committed);
        self.count_custody();
        committed
    }

    /// `workspace`'s writes in this device's custody: id, bytes, channel and retention.
    pub(super) fn custody(&self, workspace: tilo_desktop_model::WorkspaceId) -> Vec<CustodyEntry> {
        self.replicas
            .owner()
            .with_outgoing(workspace, web_time::SystemTime::now(), |_, outgoing| {
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
        let sent: BTreeSet<[u8; 32]> = self.sent.values().map(|(_, id, _)| *id).collect();
        let mut delivered = true;
        for (id, body, channel, retention) in self.custody(workspace) {
            if sent.contains(&id) {
                continue;
            }
            let payload = match retention {
                neighbour_wire::Retention::Checkpoint => {
                    CoeditPayload::Checkpoint { channel, body }
                }
                neighbour_wire::Retention::Delta => CoeditPayload::RetainedDelta { channel, body },
            };
            let route = route_of(&payload);
            let mut op = [0_u8; 16];
            op.copy_from_slice(&id[..16]);
            let commit = Envelope::Commit {
                workspace_id,
                op,
                author_device_public_key: self.config.cert.device_public_key,
                authored_at_ns: tilo_clock::unix_now_ns(),
                delta: payload.encode().unwrap_or_default(),
            };
            if client.send(commit).is_err() {
                delivered = false;
                break;
            }
            self.sent.insert(op, (workspace, id, route));
        }
        self.count_custody();
        delivered
    }

    /// Count what this device's custody still owes its hosts, over every workspace.
    pub(super) fn count_custody(&self) {
        let pending = self
            .workspaces
            .iter()
            .filter_map(|workspace_id| self.replicas.workspace(*workspace_id))
            .map(|workspace| self.custody(workspace).len())
            .sum();
        self.update(|status| status.outgoing_pending = pending);
    }

    /// Complete `done` once every write now in this device's custody is answered by its host.
    pub(super) fn await_custody(&mut self, done: Completion) {
        let owed: BTreeSet<[u8; 32]> = self
            .workspaces
            .iter()
            .filter_map(|workspace_id| self.replicas.workspace(*workspace_id))
            .flat_map(|workspace| self.custody(workspace))
            .map(|entry| entry.0)
            .collect();
        if owed.is_empty() {
            done(Ok(()));
        } else {
            self.barriers.push((owed, None, done));
        }
    }

    /// Move each held barrier whose routes hold nothing any more on to the
    /// outbox, which now carries what those routes held.
    pub(super) fn release_barriers(&mut self) {
        let (waiting, released): (Vec<_>, Vec<_>) = std::mem::take(&mut self.held_barriers)
            .into_iter()
            .partition(|(routes, _)| {
                routes
                    .iter()
                    .any(|route| self.held_outbound.contains_key(route))
            });
        self.held_barriers = waiting;
        for (_, done) in released {
            self.await_custody(done);
        }
    }

    /// Take the host's answer to the write it answered as `op`: durable, or refused with the
    /// host's `refusal` (it can never become durable). Answers retire from custody together
    /// ([`Self::retire_answered`]).
    pub(super) fn acknowledge(&mut self, op: [u8; 16], outcome: Result<u64, String>) {
        let Some((workspace, id, route)) = self.sent.remove(&op) else {
            return;
        };
        // What the refused write would have brought, as its envelope claims it: read before the
        // entry leaves custody, so whoever holds the record withdraws exactly those changes.
        let withdrawn = match (&outcome, &route) {
            (Err(reason), Some(route)) => self
                .custody(workspace)
                .into_iter()
                .find(|entry| entry.0 == id)
                .map(|entry| PeerEvent::Refused {
                    route: route.clone(),
                    changes: tilo_peer_state::document_frame::claimed_changes(&entry.1),
                    reason: reason.clone(),
                }),
            _ => None,
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
                self.emit(&refused);
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
                complete(refused.map_or(Ok(()), Err));
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
