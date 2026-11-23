//! What a received frame becomes: the session's inbound.
//!
//! Every frame the server sends arrives here ([`Task::inbound`]) and is turned
//! into what the page sees: a workspace's `Ready`, its bootstrap, one message
//! the host relayed, or a live frame of its live session. A message the workspace's
//! governance or a record's write is taken into the session's replica, which
//! decides it; a write it cannot decide yet waits there for its keys, its app
//! version's surface or the workspace's governance, and the page hears of a
//! record once the replica holds a change of it ([`Task::landed`]).

use super::*;
use tilo_server_protocol::WorkspaceId;

/// Governance a workspace's host sent as consecutive frames, not admitted yet:
/// admitted together when something else arrives or the frames already here
/// are read ([`Task::admit_governance`]).
pub(super) struct GovernanceBatch {
    workspace_id: WorkspaceId,
    author: [u8; 32],
    workspace: String,
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
        self.outbound_due |= self.held.lock().contains_key(route);
        if !self.reads_as_member(route) {
            self.unadmitted.insert(route.clone(), author);
            return;
        }
        // A record this replica does not hold has nothing that changed: a
        // route whose keys arrived while its first write is still on its way
        // is not a landing, and a reader told of it would have its read
        // refused. It lands when its first write does.
        let held = route.workspace_id().is_ok_and(|workspace| {
            self.engine.owner().with_current(workspace, |view| {
                Ok(view.peer().holds_document(&route.app_id, &route.document))
            }) == Ok(true)
        });
        if !held {
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
        self.reports.change(|status| {
            status.serving.insert(workspace_id);
        });
        true
    }

    /// The records of `workspace_id` that landed before its governance admitted
    /// this device, landed now that it does.
    fn land_admitted(&mut self, workspace_id: WorkspaceId) {
        if self.admitted_here.contains(&workspace_id)
            || !self.replicas.admits_this_device(workspace_id)
        {
            return;
        }
        self.admitted_here.insert(workspace_id);
        self.reports.change(|status| {
            status.serving.insert(workspace_id);
        });
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

    /// The server no longer lets this device into `workspace_id`, for
    /// `refusal`: its owner deleted it, or this device may not.
    fn not_authorised(
        &mut self,
        workspace_id: WorkspaceId,
        refusal: &tilo_server_protocol::refusal::Refusal,
    ) {
        use tilo_server_protocol::refusal::Refusal;
        match refusal {
            Refusal::WorkspaceDeleted => self.left(workspace_id, WORKSPACE_DELETED),
            // Taken out of the workspace (removed, or this device no longer counts as theirs):
            // the workspace stops letting this session in, as a deleted one does, and the page
            // hears it is no longer a member rather than a permission it could ask for.
            Refusal::NotAMember(_) => self.left(workspace_id, NOT_ADMITTED),
            refusal => self.fail("permission-required", refusal.to_string()),
        }
    }

    /// The server lets this session into `workspace_id` no more, for `why` (its owner deleted
    /// it, `workspace-deleted`, or took this person out, `not-admitted`). The session stops
    /// following it and says why for the page or layout to act on; a session holding nothing
    /// else ends there, as a refusal ends one.
    fn left(&mut self, workspace_id: WorkspaceId, (code, message): (&'static str, &str)) {
        self.workspaces.remove(&workspace_id);
        self.live_sessions_due.remove(&workspace_id);
        self.placing.remove(&workspace_id);
        // Its records are opened on no later subscription: none of them is answered.
        self.opened_records
            .retain(|(workspace, _), _| *workspace != workspace_id);
        self.fail(code, message);
        let nothing_left = self.workspaces.is_empty();
        self.emptied = nothing_left;
        self.reports.change(|status| {
            status.workspaces.remove(&workspace_id);
            status.serving.remove(&workspace_id);
            if nothing_left {
                status.phase = TransportPhase::Failed;
            }
        });
    }

    /// The server admitted this session to `workspace_id` and sent its replay;
    /// it sends records `when_opened` in a public workspace.
    fn ready(&mut self, workspace_id: WorkspaceId, when_opened: bool) {
        self.emptied = false;
        self.reports.change(|status| {
            status.workspaces.insert(workspace_id, true);
        });
        // The workspace is ready once what the host replayed before it is in the replica:
        // whoever waits for it reads every write the replay carried.
        self.keep_arrived();
        // What the host was not ready for before goes on the next pass of ownership.
        if let Some(sending) = self.sending.get_mut(&workspace_id) {
            sending.ready();
        }
        // A host is dialled by its certified key, so a session that got this far holds a valid one.
        if let Ok(peer) =
            iroh_base::EndpointId::from_bytes(&self.config.server_cert.device_public_key)
        {
            self.reports.emit(&crate::Event::HostReady {
                ws: workspace_id,
                peer,
                documents_when_opened: when_opened,
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
                documents_when_opened,
                ..
            } => self.ready(workspace_id, documents_when_opened),
            Envelope::NotAuthorised {
                workspace_id,
                refusal,
            } => self.not_authorised(workspace_id, &refusal),
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
            // This session's token for its host's fetch proxy, which dies
            // with the connection.
            Envelope::FetchGrant { token } => self.grant_fetch_proxy(&token),
            Envelope::PersonalList { list } => self.personal_list(&list),
            said @ Envelope::HostSaid { .. } => self.heard_from_host(&said),
            Envelope::DocumentOpened {
                workspace_id,
                channel,
                answer,
            } => self.resource_opened(workspace_id, channel, answer),
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
                    document: record,
                },
            ),
            Envelope::WorkspacesReply { request_id, frame } => {
                self.workspaces_reply(request_id, &frame);
            }
            answer @ Envelope::AgentSessionAnswer { .. } => self.agent_run_answered(answer),
            Envelope::Committed { .. } => {}
            Envelope::CommitRefused { refusal, .. } => {
                // A host not ready for the write yet takes it once it is: nothing failed.
                if !refusal.passes_once_ready() {
                    self.fail("protocol-rejected", refusal.to_string());
                }
            }
            content @ Envelope::DocumentContent { .. } => self.document_content(content, now),
            envelope => {
                if let Some(event) = addressed(
                    envelope,
                    self.config.server_cert.device_public_key,
                    self.config.server_cert.user_public_key,
                ) {
                    // A request another device addressed to this one is this
                    // device's to answer: handed to what serves them, and never
                    // passed on as what the caller side waits on, which is an
                    // answer. A session that serves none leaves it unserved.
                    if let Some(inbound) = self.reports.inbound()
                        && let Event::Remote(Heard::Action {
                            workspace_id,
                            user,
                            from,
                            frame,
                        }) = &event
                        && tilo_actions::remote::RemoteActionFrame::decode(frame)
                            .is_ok_and(|decoded| decoded.message.is_request())
                    {
                        inbound.take(*workspace_id, *user, *from, frame.clone());
                        return;
                    }
                    self.reports.emit(&event);
                }
            }
        }
    }

    /// The host says the person's workspaces changed: the list as it is now,
    /// for whoever shows it beside the workspaces this device remembered.
    fn personal_list(&self, list: &[u8]) {
        match serde_json::from_slice::<tilo_server_session::workspaces::PersonalList>(list) {
            Ok(list) => self.reports.emit(&crate::Event::Personal(Box::new(list))),
            Err(error) => {
                tracing::warn!(%error, "the host's workspace list did not decode");
            }
        }
    }

    /// A coedit delta: its body, when it carries one, is the workspace's
    /// content from `author`.
    ///
    /// Every write a host sends was signed by its author when it was made,
    /// whether the host relays it as it arrives, replays a newcomer what its
    /// store kept or answers for a record a device opens
    /// ([`Task::open_resource`]): it is decided as of its own timestamp, so a
    /// chat written an hour ago and opened now reads. Whether a write in the
    /// clear is a record's base or a downgrade is where it stands in the
    /// record, before its seal or after, which the replica decides.
    fn delta(&mut self, workspace_id: WorkspaceId, delta: &[u8], author: [u8; 32]) {
        match CoeditPayload::decode(delta) {
            Some(
                CoeditPayload::Checkpoint { body, .. } | CoeditPayload::RetainedDelta { body, .. },
            ) => self.inbound_body(workspace_id, &body, author),
            None => {}
        }
    }

    /// The server answered this subscription with the workspace's membership
    /// answer: the replica is installed from it, or one the page kept for the
    /// same server and device stands. Either way the page reads what that
    /// replica's governance says now — a kept replica's member list is no
    /// change on this connection, and a page that waited for one named nobody
    /// until the workspace's governance next moved.
    fn bootstrap(&mut self, workspace_id: WorkspaceId, bootstrap: &[u8]) {
        self.keep_arrived();
        match self
            .replicas
            .bootstrap(workspace_id, bootstrap, tilo_clock::wall())
        {
            Ok(()) => self.follow_governance(workspace_id),
            Err(error) => self.fail("protocol-rejected", error),
        }
    }

    /// A surface this page asked for is the peer's, not a record's content: it is kept for
    /// deciding, and what waited for it is decided at the top of the session loop.
    fn document_content(&mut self, envelope: Envelope, now: u64) {
        let Envelope::DocumentContent { ref frame, .. } = envelope else {
            return;
        };
        let device = self.config.cert.device_public_key;
        if let Some(arrived) = self.surface_wants.piece(&device, frame, now) {
            self.surfaces_arrived |= crate::surfaces::keep(&arrived.hash, arrived.bytes);
        } else if let Some(event) = addressed(
            envelope,
            self.config.server_cert.device_public_key,
            self.config.server_cert.user_public_key,
        ) {
            self.reports.emit(&event);
        }
    }

    /// Follow what `workspace_id`'s governance says now: who its members are,
    /// and the key group's commits that waited for a member it now admits.
    pub(super) fn follow_governance(&mut self, workspace_id: WorkspaceId) {
        self.live_sessions_due.insert(workspace_id);
        self.land_admitted(workspace_id);
        if let Some((workspace, members)) = self.replicas.members(workspace_id) {
            if self.reports.status().workspace_members.get(&workspace) != members.as_ref() {
                self.reports.change(|status| match members {
                    Some(members) => {
                        _ = status.workspace_members.insert(workspace.clone(), members);
                    }
                    None => _ = status.workspace_members.remove(&workspace),
                });
            }
            let devices = self.replicas.member_devices(workspace_id);
            if self.reports.status().workspace_devices.get(&workspace) != devices.as_ref() {
                self.reports.change(|status| match devices {
                    Some(devices) => {
                        _ = status.workspace_devices.insert(workspace.clone(), devices);
                    }
                    None => _ = status.workspace_devices.remove(&workspace),
                });
            }
            let calls = self.replicas.call_rules(workspace_id);
            if self.reports.status().workspace_calls.get(&workspace) != calls.as_ref() {
                self.reports.change(|status| match calls {
                    Some(calls) => _ = status.workspace_calls.insert(workspace.clone(), calls),
                    None => _ = status.workspace_calls.remove(&workspace),
                });
            }
            let names = self.replicas.member_names(workspace_id);
            if self.reports.status().workspace_names.get(&workspace) != names.as_ref() {
                self.reports.change(|status| match names {
                    Some(names) => _ = status.workspace_names.insert(workspace.clone(), names),
                    None => _ = status.workspace_names.remove(&workspace),
                });
            }
        }
        self.pump_channels(true);
        // A record opened before the governance admitted it waits with its
        // key group unopened: opening it here asks the members for the keys
        // the writes it holds wait on.
        let unopened: Vec<DocumentRoute> = self.held.lock().keys().cloned().collect();
        for route in unopened {
            self.open_channel(&route);
        }
    }

    /// One retained or live message the host relayed to `workspace_id`:
    /// the workspace's governance (a member's certificate, the workspace document)
    /// is admitted through the workspace's replica; the key group's governance and
    /// documents go on.
    pub(super) fn inbound_body(
        &mut self,
        workspace_id: WorkspaceId,
        body: &[u8],
        author: [u8; 32],
    ) {
        match neighbour_wire::NeighbourMsg::decode(body) {
            Ok(neighbour_wire::NeighbourMsg::SyncResp { changes, workspace }) => {
                self.gather_governance(workspace_id, author, workspace, changes);
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
        workspace_id: WorkspaceId,
        author: [u8; 32],
        workspace: String,
        changes: Vec<Vec<u8>>,
    ) {
        let bytes: usize = changes.iter().map(Vec::len).sum();
        let joins = self.governance_batch.as_ref().is_some_and(|batch| {
            batch.workspace_id == workspace_id
                && batch.author == author
                && batch.workspace == workspace
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
                workspace,
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
            workspace,
            frames,
            ..
        } = batch;
        if frames.len() > 1 {
            let message = neighbour_wire::NeighbourMsg::SyncResp {
                changes: frames.iter().flatten().cloned().collect(),
                workspace: workspace.clone(),
            };
            if self.admit_governance_message(workspace_id, author, message) {
                return;
            }
        }
        for changes in frames {
            let message = neighbour_wire::NeighbourMsg::SyncResp {
                changes,
                workspace: workspace.clone(),
            };
            self.admit_governance_message(workspace_id, author, message);
        }
    }

    /// Admit `message`, governance `author` sent `workspace_id`, through the
    /// workspace's replica: whether the replica took it.
    fn admit_governance_message(
        &mut self,
        workspace_id: WorkspaceId,
        author: [u8; 32],
        message: neighbour_wire::NeighbourMsg,
    ) -> bool {
        // Governance the replica refuses is not the workspace's. A device that
        // cannot decide it at all leaves the workspace without its governance
        // and every write held, so it says so.
        let report = match self
            .replicas
            .admit(workspace_id, author, message, tilo_clock::wall())
        {
            Ok(report) => report,
            Err(error) => {
                if error.is_device_failure() {
                    tracing::warn!(
                        %workspace_id,
                        %error,
                        "this device cannot decide the workspace's governance"
                    );
                } else {
                    tracing::debug!(
                        %workspace_id,
                        author = %hex::encode(author),
                        %error,
                        "governance the replica refused"
                    );
                }
                return false;
            }
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
            // A channel's governance, and another device's request to join its group, are taken
            // in by the key service the session runs: the same entry a link frame takes, so a
            // member's request is answered from a session as it is from a link. The device's own
            // key service hears too, when it has a link of its own.
            message @ (neighbour_wire::NeighbourMsg::MlsGovernance { .. }
            | neighbour_wire::NeighbourMsg::MlsAccessRequest { .. }) => {
                if let Some(inbound) = tilo_workspace_keys::InboundGovernance::from_message(message)
                    && let Some(governance) = &self.governance
                {
                    governance.inbound.lock_recover().push(inbound);
                    self.pump_channels(false);
                }
            }
            ref message @ neighbour_wire::NeighbourMsg::AppDocument {
                ref app_id,
                ref cluster_id,
                ref document_id,
                ref envelope,
                ref readers,
                ..
            } => {
                let route = DocumentRoute {
                    app_id: app_id.clone(),
                    cluster_id: cluster_id.clone(),
                    document: document_id.clone(),
                };
                // A workspace the host resumes after it lagged can replay an entry
                // its live deliveries already carried; the page takes it once.
                let entry = tilo_server_protocol::compaction::entry_id(body);
                if self.received.contains(&entry) {
                    return;
                }
                let Ok(workspace_id) = route.workspace_id() else {
                    return;
                };
                // The channel carries the record's content keys, and which devices read it is
                // its workspace's governance, not any one write: take part in it for every
                // write that arrives, so a record whose writes are all sealed asks for them.
                if let Some(readers) = readers {
                    let Ok(scope) = route.key_scope_for(Some(readers)) else {
                        return;
                    };
                    if let Some(keys) = self.engine.keys().get()
                        && let Some(group) = keys.reader_group(workspace_id, readers)
                    {
                        keys.access(group);
                        if envelope.encryption.is_some_and(|metadata| {
                            metadata.key_group(workspace_id) == scope.group()
                        }) && let Some(group) = keys.register_scope(scope)
                        {
                            keys.access(group);
                        }
                    }
                }
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
    pub(super) fn admitted(&mut self, workspace_id: WorkspaceId, client: &ServerClient) {
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
        report: Result<tilo_peer_state::AdmissionReport, tilo_peer_state::AdmissionError>,
    ) {
        let Arrived {
            author,
            route,
            entry,
            surface,
            message,
            ..
        } = document;
        let compaction = matches!(
            &message,
            neighbour_wire::NeighbourMsg::AppDocument { envelope, .. }
                if envelope.kind == collab_types::OpKind::Compaction
        );
        let report = match report {
            Ok(report) => report,
            // This computer could not decide the write: nothing says it was
            // refused, so nothing is counted as refused.
            Err(error) if error.is_device_failure() => {
                tracing::warn!(
                    %error,
                    "this computer could not decide a workspace write; nothing was refused"
                );
                return;
            }
            Err(error) => {
                let why = error.to_string();
                // A compaction refused leaves this device behind in its document: nothing
                // written to it afterwards reads until one is taken.
                if compaction {
                    self.fell_behind(&route, &why, tilo_clock::unix_now_ms());
                }
                self.refused = (self.refused.0.saturating_add(1), why);
                return;
            }
        };
        let mut landed = BTreeMap::new();
        if tilo_peer_state::owner::took_in(&report, true) {
            if compaction {
                self.caught_up(&route);
            }
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
                    document: dropped.record.clone(),
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

    /// The replica refused a compaction of `route`, saying `why`: this device holds only part
    /// of what the compaction covers and could not take it with its keys either. It says so
    /// where it says what it holds undecided, and asks its workspace for the document again
    /// ([`Self::ask_behind_again`]).
    pub(super) fn fell_behind(&mut self, route: &DocumentRoute, why: &str, now_ms: u64) {
        if !self.behind.contains_key(route) {
            tracing::warn!(
                app = %route.app_id,
                document = %route.document,
                %why,
                "this device holds less of a document than its workspace compacted and could not take the compaction; it asks for the document again"
            );
        }
        let behind = self.behind.entry(route.clone()).or_insert_with(|| Behind {
            why: String::new(),
            asked: 0,
            next_ms: now_ms.saturating_add(BEHIND_ASKED_AFTER_MS),
            refused_since: true,
        });
        why.clone_into(&mut behind.why);
        behind.refused_since = true;
        self.held_inbound.insert(
            route.clone(),
            format!("behind a compaction it could not take: {why}"),
        );
    }

    /// A compaction of `route` was taken: this device is behind in it no more.
    pub(super) fn caught_up(&mut self, route: &DocumentRoute) {
        if self.behind.remove(route).is_some() {
            self.held_inbound.remove(route);
            tracing::info!(
                app = %route.app_id,
                document = %route.document,
                "the compaction this device was behind was taken"
            );
        }
    }

    /// Ask the workspaces again, at `now_ms`, for each document this device is behind in whose
    /// time has come ([`Self::replay_surfaced`] sends the asking): after
    /// [`BEHIND_ASKED_AFTER_MS`] the first time, then twice as long each time and never longer
    /// than [`BEHIND_ASKED_AT_MOST_EVERY_MS`], and said in the log each time.
    pub(super) fn ask_behind_again(&mut self, now_ms: u64) {
        for (route, behind) in &mut self.behind {
            if now_ms < behind.next_ms {
                continue;
            }
            behind.asked = behind.asked.saturating_add(1);
            behind.refused_since = false;
            let wait = BEHIND_ASKED_AFTER_MS
                .saturating_mul(2_u64.saturating_pow(behind.asked.min(32)))
                .min(BEHIND_ASKED_AT_MOST_EVERY_MS);
            behind.next_ms = now_ms.saturating_add(wait);
            tracing::warn!(
                app = %route.app_id,
                document = %route.document,
                asked = behind.asked,
                why = %behind.why,
                "a document this device is behind in is asked of its workspace again"
            );
            self.replays.insert(route.clone());
        }
    }

    /// The workspace answered that it sent all it holds of the document `digest` names in
    /// `workspace_id`: one this device was behind in and asked for again, none of whose
    /// compactions was refused this time, is behind no more.
    pub(super) fn answered_behind(&mut self, workspace_id: WorkspaceId, digest: [u8; 32]) {
        let settled: Vec<DocumentRoute> = self
            .behind
            .iter()
            .filter(|(route, behind)| {
                behind.asked > 0
                    && !behind.refused_since
                    && route.workspace_id().ok() == Some(workspace_id)
                    && neighbour_wire::app_document_group(&route.app_id, &route.document) == digest
            })
            .map(|(route, _)| route.clone())
            .collect();
        for route in settled {
            self.caught_up(&route);
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
    pub(super) fn ask_surfaces(&mut self, client: &ServerClient) {
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

    /// Let each record whose writes waited for an app surface that is here now, and each one the
    /// replica asked to hear again, wait for its link: a workspace's records reach a device over
    /// its link to the workspace, which brings them again, never a session, so the host is asked
    /// for nothing here.
    pub(super) fn replay_surfaced(&mut self) {
        self.awaiting_surface
            .retain(|surface, (_, route)| !crate::surfaces::holds(&route.app_id, surface));
        self.replays.clear();
    }
}

/// What the host addressed to this device that it only passes on: an
/// action frame or what became of one, a piece of content, a member's
/// coming or going. `host` is the host's own device, which answers its own operations.
fn addressed(envelope: Envelope, host: [u8; 32], host_user: [u8; 32]) -> Option<Event> {
    match envelope {
        Envelope::HostAction { frame } => Some(Event::Remote(Heard::Action {
            workspace_id: HOST_ITSELF,
            user: host_user,
            from: host,
            frame,
        })),
        Envelope::RemoteAction {
            workspace_id,
            from_device_public_key,
            from_user_public_key,
            frame,
            ..
        } => Some(Event::Remote(Heard::Action {
            workspace_id,
            user: from_user_public_key,
            from: from_device_public_key,
            frame,
        })),
        Envelope::DocumentContent {
            workspace_id,
            from_device_public_key,
            frame,
            ..
        } => Some(Event::DocumentContent {
            ws: workspace_id,
            from: from_device_public_key,
            frame,
        }),
        Envelope::RemoteActionUndeliverable {
            workspace_id,
            to_device_public_key,
            refusal,
        } => Some(Event::Remote(Heard::Undeliverable {
            workspace_id,
            to: to_device_public_key,
            refusal,
        })),
        _ => None,
    }
}
