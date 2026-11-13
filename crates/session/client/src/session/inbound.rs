//! What a received frame becomes: the session's inbound.
//!
//! Every frame the host sends arrives here ([`Task::inbound`]) and is turned
//! into what the page sees: a workspace's `Ready`, its bootstrap, one message
//! the host relayed, or a live frame of its plane. A message the workspace's
//! governance or a record's write is taken into the session's replica, which
//! decides it; a write it cannot decide yet waits there for its keys, its app
//! version's surface or the workspace's governance, and the page hears of a
//! record once the replica holds a change of it ([`Task::landed`]).

use super::*;

/// Governance a workspace's host sent as consecutive frames, not admitted yet:
/// admitted together when something else arrives or the frames already here
/// are read ([`Task::admit_governance`]).
pub(super) struct GovernanceBatch {
    workspace_id: u64,
    author: [u8; 32],
    room: String,
    /// Each frame's changes, in the order they came.
    frames: Vec<Vec<Vec<u8>>>,
    bytes: usize,
}

/// The most governance one batch gathers before it is admitted.
const GOVERNANCE_BATCH_BYTES: usize = 8 * 1024 * 1024;

impl Task {
    /// Tell this device's readers that `route` changed in its replica, once
    /// the replica's governance admits this device: they read the record as
    /// this device, and a read before its own admission arrived would be
    /// refused as a stranger's and not be asked again. Until then the record
    /// waits, and lands as the governance admitting this device does
    /// ([`Self::follow_governance`]).
    pub(super) fn landed(&mut self, route: &DocumentRoute, author: Option<[u8; 32]>) {
        // This device's writes that waited for the record to change go now.
        self.outbound_due |= self.engine.held_writes().lock().contains_key(route);
        if !self.reads_as_member(route) {
            self.unadmitted.insert(route.clone(), author);
            return;
        }
        self.engine.arrivals().landed();
        self.reports.emit(&crate::Event::Landed {
            author,
            route: route.clone(),
        });
    }

    /// Whether this device reads `route`'s workspace as a member: its replica's
    /// governance admitted it on this session. Once it did, every landing goes
    /// to the readers, and a read the workspace refuses later is refused there.
    fn reads_as_member(&mut self, route: &DocumentRoute) -> bool {
        let Ok(workspace_id) = route.workspace_id() else {
            return true;
        };
        if self.admitted_here.contains(&workspace_id) {
            return true;
        }
        if !self.replicas.admits_this_device(workspace_id) {
            return false;
        }
        self.admitted_here.insert(workspace_id);
        true
    }

    /// The records of `workspace_id` that landed before its governance admitted
    /// this device, landed now that it does.
    fn land_admitted(&mut self, workspace_id: u64) {
        if self.admitted_here.contains(&workspace_id)
            || !self.replicas.admits_this_device(workspace_id)
        {
            return;
        }
        self.admitted_here.insert(workspace_id);
        let waiting: Vec<(DocumentRoute, Option<[u8; 32]>)> = self
            .unadmitted
            .extract_if(.., |route, _| {
                route.workspace_id().ok() == Some(workspace_id)
            })
            .collect();
        for (route, author) in waiting {
            self.landed(&route, author);
        }
    }

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
        self.emptied = nothing_left;
        self.reports.change(|status| {
            status.workspaces.remove(&workspace_id);
            if nothing_left {
                status.phase = TransportPhase::Failed;
            }
        });
    }

    /// The host admitted this session to `workspace_id` and sent its replay;
    /// it sends records `when_opened` in a public workspace.
    fn ready(&mut self, workspace_id: u64, when_opened: bool) {
        self.emptied = false;
        self.reports.change(|status| {
            status.workspaces.insert(workspace_id, true);
        });
        // The workspace is ready once what the host replayed before it is in the replica:
        // whoever waits for it reads every write the replay carried.
        self.keep_arrived();
        // A host is dialled by its certified key, so a session that got this far holds a valid one.
        if let Ok(peer) =
            iroh_base::EndpointId::from_bytes(&self.config.host_cert.device_public_key)
        {
            self.reports.emit(&crate::Event::HostReady {
                ws: workspace_id,
                peer,
                records_when_opened: when_opened,
            });
        }
    }

    pub(super) fn inbound(&mut self, envelope: Envelope) {
        let now = tilo_clock::unix_now_ms();
        self.reports
            .change(|status| status.last_contact_at_ms = Some(now));
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
            } => self.bootstrap(workspace_id, &bootstrap),
            Envelope::Delta {
                workspace_id,
                author_device_public_key,
                delta,
                ..
            } => self.delta(workspace_id, &delta, author_device_public_key),
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
            content @ Envelope::RecordContent { .. } => self.record_content(content, now),
            envelope => {
                if let Some(event) = addressed(envelope, self.config.host_cert.device_public_key) {
                    self.reports.emit(&event);
                }
            }
        }
    }

    /// A coedit delta: its body, when it carries one, is the workspace's
    /// content from `author`.
    ///
    /// Every write a host sends was signed by its author when it was made,
    /// whether the host relays it as it arrives, replays a newcomer what its
    /// store kept or answers for a record a device opens
    /// ([`Task::open_record`]): it is decided as of its own timestamp, so a
    /// chat written an hour ago and opened now reads. Whether a write in the
    /// clear is a record's base or a downgrade is where it stands in the
    /// record, before its seal or after, which the replica decides.
    fn delta(&mut self, workspace_id: u64, delta: &[u8], author: [u8; 32]) {
        match CoeditPayload::decode(delta) {
            Some(
                CoeditPayload::Checkpoint { body, .. } | CoeditPayload::RetainedDelta { body, .. },
            ) => self.inbound_body(workspace_id, &body, author),
            None => {}
        }
    }

    fn bootstrap(&mut self, workspace_id: u64, bootstrap: &[u8]) {
        self.keep_arrived();
        match self
            .replicas
            .bootstrap(workspace_id, bootstrap, tilo_clock::wall())
        {
            Ok(true) => self.follow_governance(workspace_id),
            Ok(false) => {}
            Err(error) => self.fail("protocol-rejected", error),
        }
    }

    /// A surface this page asked for is the peer's, not a record's content: it is kept for
    /// deciding, and what waited for it is decided at the top of the session loop.
    fn record_content(&mut self, envelope: Envelope, now: u64) {
        let Envelope::RecordContent { ref frame, .. } = envelope else {
            return;
        };
        let device = self.config.cert.device_public_key;
        if let Some(arrived) = self.surface_wants.piece(&device, frame, now) {
            self.surfaces_arrived |= crate::surfaces::keep(&arrived.hash, arrived.bytes);
        } else if let Some(event) = addressed(envelope, self.config.host_cert.device_public_key) {
            self.reports.emit(&event);
        }
    }

    /// Follow what `workspace_id`'s governance says now: who its members are,
    /// and the channel commits that waited for a participant it now admits.
    pub(super) fn follow_governance(&mut self, workspace_id: u64) {
        self.planes_due.insert(workspace_id);
        self.land_admitted(workspace_id);
        if let Some((workspace, members)) = self.replicas.members(workspace_id) {
            if self.reports.status().room_members.get(&workspace) != members.as_ref() {
                self.reports.change(|status| match members {
                    Some(members) => _ = status.room_members.insert(workspace.clone(), members),
                    None => _ = status.room_members.remove(&workspace),
                });
            }
            let devices = self.replicas.member_devices(workspace_id);
            if self.reports.status().room_devices.get(&workspace) != devices.as_ref() {
                self.reports.change(|status| match devices {
                    Some(devices) => _ = status.room_devices.insert(workspace.clone(), devices),
                    None => _ = status.room_devices.remove(&workspace),
                });
            }
        }
        self.pump_channels(true);
        // A record opened before the governance admitted it waits with its
        // channel unopened: opening it here asks the participants for the keys
        // the writes it holds wait on.
        let unopened: Vec<DocumentRoute> =
            self.engine.held_writes().lock().keys().cloned().collect();
        for route in unopened {
            self.open_channel(&route);
        }
    }

    /// One retained or live message the host relayed to `workspace_id`:
    /// the workspace's governance (a member's certificate, the workspace document)
    /// is admitted through the workspace's replica; channel governance and
    /// documents go on.
    pub(super) fn inbound_body(&mut self, workspace_id: u64, body: &[u8], author: [u8; 32]) {
        match postcard::from_bytes::<neighbour_wire::NeighbourMsg>(body) {
            Ok(neighbour_wire::NeighbourMsg::SyncResp { changes, room }) => {
                self.gather_governance(workspace_id, author, room, changes);
            }
            Ok(message @ neighbour_wire::NeighbourMsg::IdentityHello { .. }) => {
                self.admit_governance();
                // Governance is decided after the documents that came before it.
                self.keep_arrived();
                self.admit_governance_message(workspace_id, author, message);
            }
            Ok(message) => {
                self.admit_governance();
                self.inbound_message(body, message, author);
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
        room: String,
        changes: Vec<Vec<u8>>,
    ) {
        let bytes: usize = changes.iter().map(Vec::len).sum();
        let joins = self.governance_batch.as_ref().is_some_and(|batch| {
            batch.workspace_id == workspace_id
                && batch.author == author
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
        // Governance is decided after the documents that came before it.
        self.keep_arrived();
        let GovernanceBatch {
            workspace_id,
            author,
            room,
            frames,
            ..
        } = batch;
        if frames.len() > 1 {
            let message = neighbour_wire::NeighbourMsg::SyncResp {
                changes: frames.iter().flatten().cloned().collect(),
                room: room.clone(),
            };
            if self.admit_governance_message(workspace_id, author, message) {
                return;
            }
        }
        for changes in frames {
            let message = neighbour_wire::NeighbourMsg::SyncResp {
                changes,
                room: room.clone(),
            };
            self.admit_governance_message(workspace_id, author, message);
        }
    }

    /// Admit `message`, governance `author` sent `workspace_id`, through the
    /// workspace's replica: whether the replica took it.
    fn admit_governance_message(
        &mut self,
        workspace_id: u64,
        author: [u8; 32],
        message: neighbour_wire::NeighbourMsg,
    ) -> bool {
        // Governance the replica refuses is not the workspace's.
        let Ok(report) = self
            .replicas
            .admit(workspace_id, author, message, tilo_clock::wall())
        else {
            return false;
        };
        // Documents the replica held for this governance are decided now.
        if let Some(workspace) = self.replicas.workspace(workspace_id) {
            self.follow_report(&workspace.to_string(), &report);
        }
        if report.changed || report.metadata_changed {
            self.follow_governance(workspace_id);
        }
        true
    }

    /// Channel governance or a document `author` wrote.
    pub(super) fn inbound_message(
        &mut self,
        body: &[u8],
        message: neighbour_wire::NeighbourMsg,
        author: [u8; 32],
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
                let entry = tilo_relay_protocol::compaction::entry_id(body);
                if self.received.contains(&entry) {
                    return;
                }
                let Ok(workspace_id) = route.workspace_id() else {
                    return;
                };
                // The channel carries the record's content keys, and which devices read it is
                // its workspace's governance, not any one write: take part in it for every
                // write that arrives, so a record whose writes are all sealed asks for them.
                self.open_channel(&route);
                let surface = (envelope.kind == collab_types::OpKind::Change)
                    .then_some(envelope.surface)
                    .flatten();
                // The workspace's governance decides the write before any app reads it, with
                // what else the host sent at once ([`Self::keep_arrived`]).
                self.arrived.push(Arrived {
                    workspace_id,
                    author,
                    message: message.clone(),
                    route,
                    entry,
                    surface,
                });
            }
            _ => {}
        }
    }

    /// The host answered `Ready` for `workspace_id`: the action frames that waited for the
    /// admission go out in the order they were made.
    pub(super) fn admitted(&mut self, workspace_id: u64, client: &RelayClient) {
        self.check_admission();
        self.flush_held_actions(workspace_id, client);
    }

    /// Keep the documents that arrived in their workspaces' replicas, one owner turn for each
    /// workspace's run of them: the replica holds what it cannot decide yet, and decides it once
    /// it can.
    pub(super) fn keep_arrived(&mut self) {
        let mut arrived = std::mem::take(&mut self.arrived).into_iter().peekable();
        while let Some(first) = arrived.next() {
            let workspace_id = first.workspace_id;
            let mut run = vec![first];
            while let Some(next) = arrived.next_if(|next| next.workspace_id == workspace_id) {
                run.push(next);
            }
            let documents = run
                .iter()
                .map(|document| (document.author, document.message.clone()))
                .collect();
            match self
                .replicas
                .keep(workspace_id, documents, tilo_clock::wall())
            {
                Ok(reports) => {
                    for (document, report) in run.into_iter().zip(reports) {
                        self.kept(document, report);
                    }
                }
                Err(error) => {
                    self.refused = (self.refused.0.saturating_add(run.len() as u64), error);
                }
            }
        }
    }

    /// Act on what the replica made of `document`: tell its readers what landed, and say what
    /// it holds undecided and why.
    pub(super) fn kept(
        &mut self,
        document: Arrived,
        report: Result<tilo_peer_state::AdmissionReport, String>,
    ) {
        let Arrived {
            author,
            route,
            entry,
            surface,
            ..
        } = document;
        let report = match report {
            Ok(report) => report,
            Err(error) => {
                self.refused = (self.refused.0.saturating_add(1), error);
                return;
            }
        };
        let mut landed = BTreeMap::new();
        if tilo_peer_state::owner::took_in(&report, true) {
            if self.received.len() < MAX_DELIVERED_ENTRIES {
                self.received.insert(entry);
            }
            self.note_delivered(&route, &[entry]);
            // The replica took a write of the record in: what it held of it is decided.
            self.held_inbound.remove(&route);
            landed.insert(route.clone(), author);
        } else if let tilo_peer_state::InputDisposition::RetryLater(waiting) = report.primary() {
            self.held(&route, waiting, surface);
        }
        if report.replay_wanted {
            self.replays.insert(route.clone());
        }
        for (route, author) in landed {
            self.landed(&route, Some(author));
        }
        self.follow_report(&route.cluster_id, &report);
    }

    /// Follow what else the replica did with `report`'s input to the workspace `cluster`: tell
    /// readers of the documents it held and decided now that they landed, remember the records
    /// of writes it let go undecided, and have the host replay those once what they waited for
    /// landed.
    pub(super) fn follow_report(
        &mut self,
        cluster: &str,
        report: &tilo_peer_state::AdmissionReport,
    ) {
        // A write decided again that waits for something else now says so, and asks for an app
        // surface it waits for.
        for still in report.still_held() {
            if let neighbour_wire::NeighbourMsg::AppDocument { envelope, .. } = &still.message
                && let Some(route) = DocumentRoute::of_message(&still.message)
            {
                let surface = (envelope.kind == collab_types::OpKind::Change)
                    .then_some(envelope.surface)
                    .flatten();
                self.held(&route, &still.waiting_for, surface);
            }
        }
        for dropped in &report.summary().dropped {
            if dropped.waiting_for.is_some() && !dropped.app.is_empty() {
                self.let_go.insert(DocumentRoute {
                    app_id: dropped.app.clone(),
                    cluster_id: cluster.to_owned(),
                    record: dropped.record.clone(),
                });
            }
        }
        if report.replay_wanted {
            let let_go = std::mem::take(&mut self.let_go);
            self.replays.extend(let_go);
        }
        let landed: BTreeMap<DocumentRoute, [u8; 32]> = report
            .released()
            .iter()
            .filter_map(|released| match &released.message {
                neighbour_wire::NeighbourMsg::AppDocument { envelope, .. } => {
                    DocumentRoute::of_message(&released.message)
                        .map(|route| (route, *envelope.author.as_bytes()))
                }
                _ => None,
            })
            .collect();
        for (route, author) in landed {
            self.held_inbound.remove(&route);
            self.landed(&route, Some(author));
        }
    }

    /// Say that the replica holds a write of `route` undecided until `waiting` arrives: an app
    /// surface the write names (`surface`) is asked of its workspace.
    pub(super) fn held(
        &mut self,
        route: &DocumentRoute,
        waiting: &tilo_peer_state::MissingPrerequisite,
        surface: Option<[u8; 32]>,
    ) {
        let reason = match (waiting, surface, route.workspace_id()) {
            (tilo_peer_state::MissingPrerequisite::AppSurface, Some(surface), Ok(workspace_id)) => {
                self.awaiting_surface
                    .insert(surface, (workspace_id, route.clone()));
                format!("for its app surface {}", hex::encode(surface))
            }
            (waiting, ..) => format!("for {waiting}"),
        };
        self.held_inbound.insert(route.clone(), reason);
    }

    /// Ask the workspaces for the app surfaces their writes here wait for and this page does
    /// not have: the workspace keeps every version its history was written with, and a member
    /// that wrote under one answers with it.
    pub(super) fn ask_surfaces(&mut self, client: &RelayClient) {
        let device = self.config.cert.device_public_key;
        let now = tilo_clock::unix_now_ms();
        let wants: Vec<_> = self
            .awaiting_surface
            .iter()
            .map(|(surface, (workspace_id, route))| (*surface, *workspace_id, route.clone()))
            .collect();
        for (surface, workspace_id, route) in wants {
            if crate::surfaces::holds(&route.app_id, &surface) {
                continue;
            }
            let Some(request) = self.surface_wants.ask(
                &device,
                (),
                (&route.app_id, &route.cluster_id),
                &hex::encode(surface),
                now,
            ) else {
                continue;
            };
            self.want_content(workspace_id, request, Some(client));
        }
    }

    /// Ask the host to replay each record whose writes waited for an app surface that is here
    /// now, and each one the replica asked to hear again: its writes are decided again.
    pub(super) fn replay_surfaced(&mut self, client: &RelayClient) {
        let arrived: Vec<[u8; 32]> = self
            .awaiting_surface
            .iter()
            .filter(|(surface, (_, route))| crate::surfaces::holds(&route.app_id, surface))
            .map(|(surface, _)| *surface)
            .collect();
        for surface in arrived {
            if let Some((_, route)) = self.awaiting_surface.remove(&surface) {
                self.replays.insert(route);
            }
        }
        for route in std::mem::take(&mut self.replays) {
            let Ok(workspace_id) = route.workspace_id() else {
                continue;
            };
            let channel = neighbour_wire::app_document_channel(&route.app_id, &route.record);
            if client
                .send(Envelope::OpenRecord {
                    workspace_id,
                    channel,
                })
                .is_err()
            {
                self.replays.insert(route);
                return;
            }
        }
    }
}

/// What the host addressed to this device that it only passes on: an
/// action frame or what became of one, a piece of content, a member's
/// coming or going. `host` is the host's own device, which answers its own operations.
fn addressed(envelope: Envelope, host: [u8; 32]) -> Option<Event> {
    match envelope {
        Envelope::HostAction { frame } => Some(Event::Remote(Heard::Action {
            workspace_id: HOST_ITSELF,
            from: host,
            frame,
        })),
        Envelope::RemoteAction {
            workspace_id,
            from_device_public_key,
            frame,
            ..
        } => Some(Event::Remote(Heard::Action {
            workspace_id,
            from: from_device_public_key,
            frame,
        })),
        Envelope::RecordContent {
            workspace_id,
            from_device_public_key,
            frame,
            ..
        } => Some(Event::RecordContent {
            ws: workspace_id,
            from: from_device_public_key,
            frame,
        }),
        Envelope::RemoteActionUndeliverable {
            workspace_id,
            to_device_public_key,
            reason,
        } => Some(Event::Remote(Heard::Undeliverable {
            workspace_id,
            to: to_device_public_key,
            reason,
        })),
        _ => None,
    }
}
