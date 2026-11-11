//! What a received frame becomes: the session's inbound.
//!
//! Every frame the host sends arrives here ([`Task::inbound`]) and is turned
//! into what the page sees: a workspace's `Ready`, its bootstrap, one message
//! the host relayed, or a live frame of its plane. A message is decided
//! against its workspace's governance and its app's surface; a document that
//! cannot be decided yet is held with the reason it waits ([`HoldReason`])
//! until its route's keys, its app version's surface or the workspace's
//! governance is here, and is decided again ([`Task::decide_held`]). Holding
//! is bounded per route, and a hold that filled asks the host to replay the
//! record instead.

use super::*;

/// Why a document waits undecided.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum HoldReason {
    /// Its route's keys, which carry it sealed.
    Keys,
    /// The app version it was written with, by that version's surface hash:
    /// this page asks its workspace for it, and decides the document when it
    /// arrives.
    Surface([u8; 32]),
    /// The workspace's governance: its author's certificate, the workspace's
    /// document, or the app version the workspace runs.
    Governance,
}

/// A frame held for its route, with its authenticated author, whether the
/// host was replaying it, and what it waits for: its record's history is
/// admitted where a live write is not, and a frame decided again keeps that
/// verdict.
pub(super) struct HeldFrame {
    pub(super) body: Vec<u8>,
    pub(super) author: [u8; 32],
    pub(super) retained: bool,
    pub(super) reason: HoldReason,
}

impl HeldFrame {
    /// Whether this frame waits for an app surface an arrival can supply.
    fn waits_for_surface(&self) -> bool {
        matches!(self.reason, HoldReason::Surface(_))
    }
}

/// Why `envelope`, a write this page cannot decide yet, waits: for the app
/// version it was written with, a change naming a surface the page holds not
/// (it asks the workspace for it), or for its workspace's governance.
fn hold_reason(route: &DocumentRoute, envelope: &collab_types::SignedOp) -> HoldReason {
    let surface = (envelope.kind == collab_types::OpKind::Change)
        .then_some(envelope.surface)
        .flatten();
    match surface {
        Some(surface) if !crate::surfaces::holds(&route.app_id, &surface) => {
            HoldReason::Surface(surface)
        }
        _ => HoldReason::Governance,
    }
}

/// Governance a workspace's host sent as consecutive frames, not admitted yet:
/// admitted together when something else arrives or the frames already here
/// are read ([`Task::admit_governance`]).
pub(super) struct GovernanceBatch {
    workspace_id: u64,
    author: [u8; 32],
    retained: bool,
    room: String,
    /// Each frame's changes, in the order they came.
    frames: Vec<Vec<Vec<u8>>>,
    bytes: usize,
}

/// The most governance one batch gathers before it is admitted.
const GOVERNANCE_BATCH_BYTES: usize = 8 * 1024 * 1024;

impl Task {
    /// The host no longer lets this device into `workspace_id`, for
    /// `reason`: its owner deleted it, or this device may not.
    fn not_authorised(&mut self, workspace_id: u64, reason: String) {
        if reason == tilo_relay_protocol::refusal::WORKSPACE_DELETED {
            self.workspace_deleted(workspace_id);
        } else {
            self.fail("permission-required", reason);
        }
    }

    /// The host says `workspace_id`'s owner deleted it: it is gone for good.
    /// The session stops following it, and says so (`workspace-deleted`) for
    /// the page or desk to forget it; a session holding nothing else ends
    /// there, as a refusal ends one.
    fn workspace_deleted(&mut self, workspace_id: u64) {
        self.workspaces.remove(&workspace_id);
        self.planes_due.remove(&workspace_id);
        self.placing.remove(&workspace_id);
        self.fail(WORKSPACE_DELETED.0, WORKSPACE_DELETED.1);
        let nothing_left = self.workspaces.is_empty();
        self.update(|status| {
            status.workspaces.remove(&workspace_id);
            if nothing_left {
                status.phase = TransportPhase::Failed;
            }
        });
    }

    /// The host admitted this session to `workspace_id` and sent its replay;
    /// it sends records `when_opened` in a public workspace.
    fn ready(&mut self, workspace_id: u64, when_opened: bool) {
        self.update(|status| {
            status.workspaces.insert(workspace_id, true);
        });
        // The replay's documents reach the page before its `Ready`, as they
        // arrived.
        self.decide_held(workspace_id);
        self.emit(&PeerEvent::Ready {
            workspace_id,
            records_when_opened: when_opened,
        });
    }

    pub(super) fn inbound(&mut self, envelope: Envelope) {
        let now = tilo_clock::unix_now_ms();
        self.update(|status| status.last_contact_at_ms = Some(now));
        if !matches!(envelope, Envelope::Delta { .. }) {
            self.admit_governance();
        }
        match envelope {
            Envelope::Ready {
                workspace_id,
                records_when_opened,
                ..
            } => self.ready(workspace_id, records_when_opened),
            Envelope::NotAuthorised {
                workspace_id,
                reason,
            } => self.not_authorised(workspace_id, reason),
            Envelope::WorkspaceBootstrap {
                workspace_id,
                bootstrap,
            } => {
                match self
                    .replicas
                    .bootstrap(workspace_id, &bootstrap, web_time::SystemTime::now())
                {
                    Ok(true) => self.follow_governance(workspace_id),
                    Ok(false) => {}
                    Err(error) => self.fail("protocol-rejected", error),
                }
            }
            Envelope::Delta {
                workspace_id,
                author_device_public_key,
                delta,
                ..
            } => {
                let Some(
                    CoeditPayload::Checkpoint { body, .. }
                    | CoeditPayload::RetainedDelta { body, .. },
                ) = CoeditPayload::decode(&delta)
                else {
                    return;
                };
                self.inbound_body(workspace_id, body, author_device_public_key);
            }
            Envelope::Position {
                workspace_id,
                position,
            } => {
                self.positions.insert(workspace_id, position);
            }
            // This session's token for its host's fetch proxy, which dies
            // with the connection.
            Envelope::FetchGrant { token } => self.grant_fetch_proxy(&token),
            said @ Envelope::HostSaid { .. } => self.heard_from_host(&said),
            Envelope::RecordOpened {
                workspace_id,
                channel,
                frames,
            } => self.record_opened(workspace_id, channel, frames),
            Envelope::CompactRequest {
                workspace_id,
                request,
                app_id,
                cluster_id,
                record,
            } => self.compaction_requested(
                workspace_id,
                request,
                DocumentRoute {
                    app_id,
                    cluster_id,
                    record,
                },
            ),
            Envelope::WorkspacesReply { request_id, frame } => self.rooms_reply(request_id, &frame),
            answer @ Envelope::AgentRunAnswer { .. } => self.agent_run_answered(answer),
            Envelope::Committed { op, seq, .. } => self.acknowledge(op, Ok(seq)),
            Envelope::CommitRefused { op, reason, .. } => {
                self.fail("protocol-rejected", reason.clone());
                self.acknowledge(op, Err(reason));
            }
            Envelope::RecordContent { ref frame, .. } => {
                // A surface this page asked for is the peer's, not a record's
                // content: it is kept for deciding, and what waited for it is
                // decided at the top of the session loop.
                let device = self.config.cert.device_public_key;
                match self
                    .surface_wants
                    .piece(&device, frame, tilo_clock::unix_now_ms())
                {
                    Some(arrived) => {
                        self.surfaces_arrived |=
                            crate::surfaces::keep(&arrived.hash, arrived.bytes);
                    }
                    None => {
                        if let Some(event) = addressed(envelope) {
                            self.emit(&event);
                        }
                    }
                }
            }
            envelope => {
                if let Some(event) = addressed(envelope) {
                    self.emit(&event);
                }
            }
        }
    }

    /// One retained or live message the host relayed to `workspace_id`:
    /// the workspace's governance (a member's certificate, the workspace document)
    /// is admitted through the workspace's replica; channel governance and
    /// documents go on.
    pub(super) fn inbound_body(&mut self, workspace_id: u64, body: Vec<u8>, author: [u8; 32]) {
        let retained = lock(&self.shared).status.workspaces.get(&workspace_id) != Some(&true);
        match postcard::from_bytes::<neighbour_wire::NeighbourMsg>(&body) {
            Ok(neighbour_wire::NeighbourMsg::SyncResp { changes, room }) => {
                self.gather_governance(workspace_id, author, retained, room, changes);
            }
            Ok(message @ neighbour_wire::NeighbourMsg::IdentityHello { .. }) => {
                self.admit_governance();
                self.admit_governance_message(workspace_id, author, retained, message);
            }
            Ok(message) => {
                self.admit_governance();
                self.inbound_message(body, message, author, retained);
            }
            Err(_) => {}
        }
    }

    /// Gather `changes`, a frame of the workspace document `author` sent, with
    /// the frames before it of the same workspace and author. A host replays a
    /// workspace's governance as its base and every change since, a frame
    /// each, and taking a frame in reads the whole governance (every member's
    /// certificate, every receipt): admitted frame by frame, the replay of a
    /// workspace of hundreds cost a newcomer seconds before its desktop.
    fn gather_governance(
        &mut self,
        workspace_id: u64,
        author: [u8; 32],
        retained: bool,
        room: String,
        changes: Vec<Vec<u8>>,
    ) {
        let bytes: usize = changes.iter().map(Vec::len).sum();
        let joins = self.governance_batch.as_ref().is_some_and(|batch| {
            batch.workspace_id == workspace_id
                && batch.author == author
                && batch.retained == retained
                && batch.room == room
                && batch.bytes.saturating_add(bytes) <= GOVERNANCE_BATCH_BYTES
        });
        if !joins {
            self.admit_governance();
        }
        let batch = self
            .governance_batch
            .get_or_insert_with(|| GovernanceBatch {
                workspace_id,
                author,
                retained,
                room,
                frames: Vec::new(),
                bytes: 0,
            });
        batch.frames.push(changes);
        batch.bytes = batch.bytes.saturating_add(bytes);
    }

    /// Admit the governance gathered so far ([`Self::gather_governance`]) as
    /// one change of the workspace's replica. Should the replica refuse it
    /// whole, each frame is admitted on its own, so a frame it refuses costs
    /// only itself, as when each was admitted as it came.
    pub(super) fn admit_governance(&mut self) {
        let Some(batch) = self.governance_batch.take() else {
            return;
        };
        let GovernanceBatch {
            workspace_id,
            author,
            retained,
            room,
            frames,
            ..
        } = batch;
        if frames.len() > 1 {
            let message = neighbour_wire::NeighbourMsg::SyncResp {
                changes: frames.iter().flatten().cloned().collect(),
                room: room.clone(),
            };
            if self.admit_governance_message(workspace_id, author, retained, message) {
                return;
            }
        }
        for changes in frames {
            let message = neighbour_wire::NeighbourMsg::SyncResp {
                changes,
                room: room.clone(),
            };
            self.admit_governance_message(workspace_id, author, retained, message);
        }
    }

    /// Admit `message`, governance `author` sent `workspace_id`, through the
    /// workspace's replica: whether the replica took it.
    fn admit_governance_message(
        &mut self,
        workspace_id: u64,
        author: [u8; 32],
        retained: bool,
        message: neighbour_wire::NeighbourMsg,
    ) -> bool {
        // Governance the replica refuses is not the workspace's.
        let admitted = self.replicas.admit(
            workspace_id,
            author,
            retained,
            message,
            web_time::SystemTime::now(),
        );
        if admitted == Ok(true) {
            self.follow_governance(workspace_id);
        }
        admitted.is_ok()
    }

    /// A held document, decided again; `retained` as it arrived.
    pub(super) fn held_body(&mut self, body: Vec<u8>, author: [u8; 32], retained: bool) {
        self.admit_governance();
        if let Ok(message) = postcard::from_bytes(&body) {
            self.inbound_message(body, message, author, retained);
        }
    }

    /// Channel governance or a document `author` wrote, `retained` while the
    /// host replays the workspace.
    pub(super) fn inbound_message(
        &mut self,
        body: Vec<u8>,
        message: neighbour_wire::NeighbourMsg,
        author: [u8; 32],
        retained: bool,
    ) {
        match message {
            message @ neighbour_wire::NeighbourMsg::MlsGovernance { .. } => {
                if let Some(inbound) =
                    tilo_channels::InboundChannelGovernance::from_message(message)
                {
                    self.governance.inbound.lock_recover().push(inbound);
                    self.pump_channels(false);
                }
            }
            ref message @ neighbour_wire::NeighbourMsg::AppDocument {
                ref app_id,
                ref cluster_id,
                ref record_id,
                ref envelope,
                ..
            } => {
                let route = DocumentRoute {
                    app_id: app_id.clone(),
                    cluster_id: cluster_id.clone(),
                    record: record_id.clone(),
                };
                // A workspace the host resumes after it lagged can replay an entry
                // its live deliveries already carried; the page takes it once.
                if self
                    .received
                    .contains(&tilo_relay_protocol::compaction::entry_id(&body))
                {
                    return;
                }
                let Ok(workspace_id) = route.workspace_id() else {
                    return;
                };
                // The workspace's governance decides the write before any app
                // sees it, and holds it while it cannot yet.
                match self.replicas.decide_document(
                    workspace_id,
                    author,
                    retained,
                    message,
                    web_time::SystemTime::now(),
                ) {
                    Ok(true) => {}
                    Ok(false) => {
                        // The channel carries the record's content keys, and
                        // which devices read it is its workspace's governance,
                        // not any one write: take part in it for a write that
                        // arrived here even when this page cannot decide it
                        // yet. A record whose every write is held would
                        // otherwise never ask for the keys the writes wait on.
                        self.open_channel(&route);
                        let reason = hold_reason(&route, envelope);
                        self.hold_document(
                            &route,
                            OpKey::of(envelope),
                            body,
                            author,
                            retained,
                            reason,
                        );
                        return;
                    }
                    Err(error) => {
                        self.refused = (self.refused.0.saturating_add(1), error.clone());
                        self.emit(&PeerEvent::DocumentRejected(error));
                        return;
                    }
                }
                self.open_channel(&route);
                match self.verdict(&route, envelope, retained) {
                    Ok(None) => self.open_document(&body, author),
                    Ok(Some(reason)) => {
                        let key = OpKey::of(envelope);
                        self.hold_document(&route, key, body, author, retained, reason);
                    }
                    Err(error) => {
                        self.refused = (self.refused.0.saturating_add(1), error.to_owned());
                        self.emit(&PeerEvent::DocumentRejected(error.to_owned()));
                    }
                }
            }
            _ => {}
        }
    }

    /// Whether the page knows what governs `route`'s channel: the host answered
    /// `Ready` for its workspace (it sent the governance it holds first, so the
    /// page's mirror of its channels is what the workspace decided), or the
    /// page holds the route's keys already. A group's participants admitted
    /// the page to the group that carries the route, so its channel is sealed
    /// and the page decides the route's replayed writes now: the record shows
    /// while the rest of the workspace is still on its way.
    pub(super) fn governance_known(&self, route: &DocumentRoute) -> bool {
        self.ciphers.contains_key(route)
            || route.workspace_id().is_ok_and(|workspace_id| {
                lock(&self.shared).status.workspaces.get(&workspace_id) == Some(&true)
            })
    }

    /// Decide again the documents held for the routes of `workspace_id`,
    /// now that its governance is known: cleartext its channels admit
    /// opens, cleartext they require sealed is refused, the rest waits on.
    pub(super) fn decide_held(&mut self, workspace_id: u64) {
        for route in self.held_routes(workspace_id) {
            self.decide_route(&route, None);
        }
    }

    /// What the write `envelope` waits for once its route's keys and the
    /// channel's governance admit it: `None` when it opens now, or the error
    /// for cleartext on a channel that requires it sealed.
    pub(super) fn verdict(
        &self,
        route: &DocumentRoute,
        envelope: &collab_types::SignedOp,
        retained: bool,
    ) -> Result<Option<HoldReason>, &'static str> {
        let Some(metadata) = &envelope.encryption else {
            let admission = self.admission(
                route,
                (
                    self.governance_known(route),
                    self.ciphers.contains_key(route),
                ),
                retained,
                None,
            );
            return match admission.admits_cleartext() {
                Some(true) => Ok(None),
                Some(false) => Err("cleartext on a channel that requires it sealed"),
                None => Ok(Some(HoldReason::Governance)),
            };
        };
        Ok(self
            .ciphers
            .get(route)
            .is_none_or(|cipher| !cipher.holds_epoch(metadata.epoch))
            .then_some(HoldReason::Keys))
    }

    /// Take every document held for `route` and decide it again: what still
    /// cannot be decided is held again with the reason it has now.
    pub(super) fn decide_route(&mut self, route: &DocumentRoute, client: Option<&RelayClient>) {
        let held = self
            .held_inbound
            .get_mut(route)
            .map(InboundHold::take)
            .unwrap_or_default();
        for (_, frame) in held {
            self.held_body(frame.body, frame.author, frame.retained);
        }
        self.recount(route);
        self.replay_owed(route, client);
    }

    /// Decide again what the surfaces that arrived were waited for: every
    /// document held for an app version this page has now.
    pub(super) fn decide_held_surfaces(&mut self, client: &RelayClient) {
        let routes: Vec<DocumentRoute> = self
            .held_inbound
            .iter()
            .filter(|(_, held)| held.held().any(|(_, frame)| frame.waits_for_surface()))
            .map(|(route, _)| route.clone())
            .collect();
        for route in routes {
            self.decide_route(&route, Some(client));
        }
    }

    /// Ask the workspaces for the app surfaces this page holds documents for
    /// and does not have: the workspace keeps every version its history was
    /// written with, and a member that wrote under one answers with it.
    pub(super) fn ask_surfaces(&mut self, client: &RelayClient) {
        let mut wants: BTreeMap<[u8; 32], (u64, String, String)> = BTreeMap::new();
        for (route, held) in &self.held_inbound {
            let Ok(workspace_id) = route.workspace_id() else {
                continue;
            };
            for (_, frame) in held.held() {
                if let HoldReason::Surface(surface) = frame.reason {
                    wants.entry(surface).or_insert_with(|| {
                        (workspace_id, route.app_id.clone(), route.cluster_id.clone())
                    });
                }
            }
        }
        let device = self.config.cert.device_public_key;
        let now = tilo_clock::unix_now_ms();
        for (surface, (workspace_id, app_id, cluster_id)) in wants {
            if crate::surfaces::holds(&app_id, &surface) {
                continue;
            }
            let Some(request) = self.surface_wants.ask(
                &device,
                (),
                (&app_id, &cluster_id),
                &hex::encode(surface),
                now,
            ) else {
                continue;
            };
            self.want_content(workspace_id, request, Some(client));
        }
    }

    /// Set `route`'s status from what it holds now, by the reason each
    /// document waits.
    pub(super) fn recount(&self, route: &DocumentRoute) {
        let mut waiting = (0_usize, 0_usize, 0_usize);
        if let Some(held) = self.held_inbound.get(route) {
            for (_, frame) in held.held() {
                let count = match frame.reason {
                    HoldReason::Keys => &mut waiting.0,
                    HoldReason::Surface(_) => &mut waiting.1,
                    HoldReason::Governance => &mut waiting.2,
                };
                *count = count.saturating_add(1);
            }
        }
        self.update(|status| {
            let status = status.routes.entry(route.clone()).or_default();
            (
                status.awaiting_keys,
                status.awaiting_surface,
                status.awaiting_governance,
            ) = waiting;
        });
    }

    /// The host answered `Ready` for `workspace_id`: what the replay brought
    /// before it and a full hold let go of is paged again, where the route
    /// now opens it, and the action frames that waited for the admission go
    /// out in the order they were made.
    pub(super) fn admitted(&mut self, workspace_id: u64, client: &RelayClient) {
        self.check_admission();
        self.flush_held_actions(workspace_id, client);
        for route in self.held_routes(workspace_id) {
            let opens = self.ciphers.contains_key(&route)
                // What the host replays is the record's history, whose
                // cleartext base opens where a live write would wait.
                || self
                    .admission(&route, (true, false), true, None)
                    .admits_cleartext()
                    == Some(true);
            if opens {
                self.replay_owed(&route, Some(client));
            }
        }
    }

    /// The routes of `workspace_id` holding documents, or owing them.
    pub(super) fn held_routes(&self, workspace_id: u64) -> Vec<DocumentRoute> {
        self.held_inbound
            .keys()
            .filter(|route| route.workspace_id().ok() == Some(workspace_id))
            .cloned()
            .collect()
    }

    /// Hold a document until the reason it names goes away: its route's keys,
    /// the app surface it was written with, or its workspace's governance.
    pub(super) fn hold_document(
        &mut self,
        route: &DocumentRoute,
        key: OpKey,
        body: Vec<u8>,
        author: [u8; 32],
        retained: bool,
        reason: HoldReason,
    ) {
        let held = self
            .held_inbound
            .entry(route.clone())
            .or_insert_with(|| InboundHold::new(MAX_HELD_DOCUMENTS));
        held.hold(
            key,
            HeldFrame {
                body,
                author,
                retained,
                reason,
            },
        );
        self.recount(route);
    }

    /// Ask the host to replay the records whose full holds let documents
    /// go, now that `route` can open them.
    pub(super) fn replay_owed(&mut self, route: &DocumentRoute, client: Option<&RelayClient>) {
        let owed = self
            .held_inbound
            .get(route)
            .is_some_and(|held| !held.owed_pages().is_empty());
        let (Some(client), Ok(workspace_id)) = (client, route.workspace_id()) else {
            return;
        };
        if !owed {
            return;
        }
        let channel = neighbour_wire::app_document_channel(&route.app_id, &route.record);
        if client
            .send(Envelope::OpenRecord {
                workspace_id,
                channel,
            })
            .is_ok()
        {
            self.owed_replays.insert(channel, route.clone());
        }
    }

    pub(super) fn open_document(&mut self, body: &[u8], author: [u8; 32]) {
        match open_frame(body, author, &mut self.ciphers).and_then(|document| match document {
            Some(document) => self.reassembly.accept(document),
            None => Ok(None),
        }) {
            Ok(Some(document)) => {
                let from: [u8; 4] = author
                    .get(..4)
                    .and_then(|b| b.try_into().ok())
                    .unwrap_or_default();
                let opened = self
                    .opened
                    .entry((document.route.clone(), from))
                    .or_default();
                *opened = opened.saturating_add(1);
                let route = document.route.clone();
                if let Some(held) = self.held_inbound.get_mut(&route) {
                    held.settle(&OpKey::of(&document.envelope));
                }
                let entries = document.entries().to_vec();
                self.emit(&PeerEvent::Document(Box::new(document)));
                for entry in &entries {
                    if self.received.len() < MAX_DELIVERED_ENTRIES {
                        self.received.insert(*entry);
                    }
                }
                self.note_delivered(&route, &entries);
            }
            Ok(None) => {}
            Err(error) => {
                self.refused = (self.refused.0.saturating_add(1), error.to_string());
                self.emit(&PeerEvent::DocumentRejected(error.to_string()));
            }
        }
    }
}
