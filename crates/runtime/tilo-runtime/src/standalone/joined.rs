//! Rooms this profile joined: enrolled replicas of rooms a Tilo device or a
//! cloud host governs. Their record is this profile's own file, never the
//! desktop session's; each room syncs over one relay session with the host
//! its enrollment pins, which also carries the room's channel governance
//! between this device and the room's steward. The room's governor applies a
//! foreign device's limits to this device there, as it does to every member.
use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime},
};
use tilo_action_broker::{
    channel_encryption::{ChannelKeyProvider, GovernanceTransport, InboundChannelGovernance},
    collab_broker::FanoutState,
    collab_core::BrokerChannels,
    collab_inbound::{self, RemoteDocument},
};
use tilo_desktop_model::{RoomId, Workspace, WorkspaceId};
use tilo_identity_host::custody::HostIdentity;
use tilo_joined_room::{
    NativeWorkspaceOwner,
    session::{self, Connect, Input, Scope},
};
use tilo_peer_state::{
    InputDisposition, NeighbourMsg,
    owner_commit::{CommitFuture, OwnerCommitRequest, OwnerCommitService},
};
use tilo_relay_session::{Inbound, RelayCoeditSession, workspace_join::WorkspaceJoinClient};
use tokio::sync::Notify;

#[path = "joined_content.rs"]
mod content;
pub(super) use content::record_content;

/// The record of every joined room, in the profile.
pub(super) const JOINED_ROOMS: &str = "joined-rooms.json";
/// How often a live session asks its host for a fresh checkpoint.
const REFRESH_INTERVAL: Duration = Duration::from_secs(30);
const REDIAL_LONGEST: Duration = Duration::from_secs(60);

#[path = "mcp_admission.rs"]
mod mcp_admission;
pub(super) use mcp_admission::SessionAdmission;

pub(super) struct Joined {
    executions: Arc<super::execution::ExecutionPins>,
    owner: NativeWorkspaceOwner,
    custody: HostIdentity,
    endpoint: iroh::Endpoint,
    /// Rooms this profile must not enroll in: its own, and those the
    /// desktop session already enrolled this device in.
    refused: BTreeSet<RoomId>,
    channels: Arc<Mutex<BrokerChannels>>,
    fanout: Arc<FanoutState>,
    sessions: Mutex<BTreeMap<RoomId, RoomSession>>,
    /// The profile's record-content service, once the broker runs.
    content: std::sync::OnceLock<Arc<tilo_action_broker::record_content::RouteContent>>,
    requesters: content::Requesters,
    /// Channel governance the sessions received, for the channel service.
    governance: Mutex<Vec<InboundChannelGovernance>>,
    /// Wakes the channel service's pump when governance arrives.
    governance_wake: Arc<super::channels::PumpWake>,
    /// This device's channel keys, once the channel service runs: the only
    /// renewer of its sealed writes that expire in custody.
    keys: ChannelKeyProvider,
}

struct RoomSession {
    /// Wakes the session to send what its owner holds for it.
    wake: Arc<Notify>,
    /// The connected session, while there is one.
    live: Option<Arc<RelayCoeditSession>>,
}

impl Joined {
    /// Open the joined rooms recorded in `profile` and hold a session for
    /// each. `endpoint` is keyed by this device's key, as hosts require.
    pub(super) fn open(
        profile: &Path,
        custody: HostIdentity,
        endpoint: iroh::Endpoint,
        refused: BTreeSet<RoomId>,
        broker: (Arc<Mutex<BrokerChannels>>, Arc<FanoutState>),
    ) -> io::Result<Arc<Self>> {
        let owner =
            tilo_joined_room::open_file(&profile.join(JOINED_ROOMS)).map_err(io::Error::other)?;
        let executions = super::execution::ExecutionPins::new(&owner);
        let joined = Arc::new(Self {
            executions,
            owner,
            custody,
            endpoint,
            refused,
            channels: broker.0,
            fanout: broker.1,
            sessions: Mutex::new(BTreeMap::new()),
            content: std::sync::OnceLock::new(),
            requesters: content::Requesters::default(),
            governance: Mutex::new(Vec::new()),
            governance_wake: Arc::default(),
            keys: ChannelKeyProvider::default(),
        });
        for room in joined.owner.rooms() {
            joined.hold_session(room);
        }
        Ok(joined)
    }

    /// This device's endpoint, which the profile's other services share.
    pub(super) fn endpoint(&self) -> iroh::Endpoint {
        self.endpoint.clone()
    }

    /// Redeem `invitation` with the host it names and start syncing the room.
    ///
    /// # Errors
    /// The invitation is not for this device, names a room this profile
    /// must not enroll in, or the host refuses or cannot be reached; a proof
    /// already sent is kept for an exact retry.
    pub(super) async fn join(self: &Arc<Self>, invitation: &str) -> Result<RoomId, String> {
        let room = self.invited_room(invitation)?;
        if self.refused.contains(&room) {
            return Err("this device takes part in that room through the desktop session".into());
        }
        let topic = room_topic(room)?;
        let enrollment = tilo_joined_room::enroll::prepare_and_complete(
            WorkspaceJoinClient::new(self.endpoint.clone()),
            self.custody.clone(),
            topic,
            None,
            invitation.to_owned(),
            self.owner.clone(),
        )
        .await?;
        self.owner.install(&enrollment, SystemTime::now())?;
        self.hold_session(room);
        Ok(room)
    }

    /// Every joined room, as a workspace its governance describes: who owns
    /// it and every capability in it.
    pub(super) fn workspaces(&self) -> Vec<Workspace> {
        self.owner
            .rooms()
            .into_iter()
            .filter_map(|room| self.current_room(room).ok().map(|(workspace, _)| workspace))
            .collect()
    }

    /// One joined room as [`Self::workspaces`] describes it, built alone:
    /// asking about one room never rebuilds every other joined room.
    pub(super) fn workspace(&self, room: RoomId) -> Option<Workspace> {
        if !self.owner.rooms().contains(&room) {
            return None;
        }
        self.current_room(room).ok().map(|(workspace, _)| workspace)
    }

    /// Whether this profile has joined any room.
    pub(super) fn is_empty(&self) -> bool {
        self.owner.rooms().is_empty()
    }

    // Use the same current custody check as an enrolled relay session. Merely
    // retaining a peer or its device key cannot authenticate a selected root.
    fn current_room(&self, room: RoomId) -> Result<(Workspace, String), String> {
        let (_, cert, _, continuity) =
            Connect::new(self.owner.clone(), room)?.material(&self.custody)?;
        self.owner.with_current(room, |view| {
            if view.continuity() != continuity || view.device() != &cert {
                return Err("room identity changed while selecting the audience".into());
            }
            let (_, _, principal) = view
                .peer()
                .permissions(cert.device_public_key, SystemTime::now())?;
            let document = view.peer().admitted_room().ok_or("no admitted room")?;
            let (name, owner, capabilities) =
                tilo_peer_state::room::extract_governed_fields(document);
            let mut workspace =
                Workspace::new(WorkspaceId(view.workspace()), name.unwrap_or_default());
            workspace.room = room;
            workspace.owner = owner;
            workspace.capabilities = capabilities;
            Ok((workspace, principal))
        })
    }

    /// Select only a room this exact profile principal and certified device
    /// currently take part in. Reading authority never joins or requests keys.
    pub(super) fn action_room(&self, actor: &str, room: &str) -> Result<String, String> {
        let room = RoomId::parse(room).ok_or("invalid MCP room")?;
        let (_, principal) = self.current_room(room)?;
        if actor != principal {
            return Err("the selected profile actor is not this room's admitted identity".into());
        }
        Ok(room.to_string())
    }

    /// The slot the channel service fills with this device's channel keys.
    pub(super) fn channel_keys(&self) -> ChannelKeyProvider {
        self.keys.clone()
    }

    /// What the sessions poke when governance arrives for the channel
    /// service, whose pump otherwise sleeps while no channel is live.
    pub(super) fn governance_wake(&self) -> Arc<super::channels::PumpWake> {
        Arc::clone(&self.governance_wake)
    }

    /// The device key of `room`'s governor, which stewards its channels.
    pub(super) fn steward_of(&self, room: RoomId) -> Option<collab_types::Ed25519PublicKey> {
        self.owner
            .with_current(room, |view| {
                Ok(collab_types::Ed25519PublicKey::from_bytes(
                    view.governor().device_public_key,
                ))
            })
            .ok()
    }

    fn invited_room(&self, invitation: &str) -> Result<RoomId, String> {
        let now = tilo_clock::try_unix_now_ns().ok_or("the clock is before the Unix epoch")?;
        let join = tilo_peer_state::join::decode_and_verify_invitation_text(
            invitation,
            Some(self.custody.device_cert.user_public_key),
            &tilo_user_identity::RevocationList::new(),
            now,
        )?;
        RoomId::parse(&join.grant().room).ok_or_else(|| "invitation names no valid room".into())
    }

    fn hold_session(self: &Arc<Self>, room: RoomId) {
        let Ok(mut sessions) = self.sessions.lock() else {
            return;
        };
        if sessions.contains_key(&room) {
            return;
        }
        let wake = Arc::new(Notify::new());
        sessions.insert(
            room,
            RoomSession {
                wake: Arc::clone(&wake),
                live: None,
            },
        );
        drop(sessions);
        let joined = Arc::clone(self);
        tilo_rt::app().spawn(async move { joined.keep_session(room, &wake).await });
    }

    /// Hold `room`'s session for as long as the room stays enrolled,
    /// dialing again after each loss.
    async fn keep_session(&self, room: RoomId, wake: &Notify) {
        let mut redial = Duration::from_secs(1);
        while let Ok(connect) = Connect::new(self.owner.clone(), room) {
            match self.run_session(connect, wake).await {
                Ok(()) => redial = Duration::from_secs(1),
                Err(error) => tracing::debug!(%error, %room, "joined room session unavailable"),
            }
            tokio::time::sleep(redial).await;
            redial = redial.saturating_mul(2).min(REDIAL_LONGEST);
        }
    }

    async fn run_session(&self, connect: Connect, wake: &Notify) -> Result<(), String> {
        let (hint, device, revocations, continuity) = connect.material(&self.custody)?;
        let room = connect.room();
        let session = Arc::new(
            session::dial(
                self.endpoint.clone(),
                room_topic(room)?,
                &hint,
                device.clone(),
                self.custody.device_signing_key.clone(),
                revocations,
            )
            .await?,
        );
        let scope = Arc::new(Scope::new(connect, hint, device, continuity));
        self.set_live(room, Some(Arc::clone(&session)));
        self.publish(&scope, &session);
        let mut replay = true;
        let mut refresh = tokio::time::interval(REFRESH_INTERVAL);
        loop {
            tokio::select! {
                inbound = session.recv() => match inbound {
                    None => break,
                    Some(Inbound::Ready { .. }) => replay = false,
                    Some(Inbound::Delta { author, data }) => {
                        if let Ok(message) = postcard::from_bytes(&data) {
                            self.admit(&scope.input(author, replay, message));
                        }
                        self.publish(&scope, &session);
                    }
                    Some(Inbound::Presence { author, payload }) => {
                        if let Ok(message) = postcard::from_bytes::<NeighbourMsg>(&payload)
                            && !self.content_wanted(room, author, &message)
                        {
                            self.receive_governance(message);
                        }
                    }
                    Some(Inbound::RecordContent { from, frame }) => {
                        self.content_arrived(room, from, &frame);
                    }
                    Some(Inbound::RemoteAction { .. } | Inbound::Live { .. } | Inbound::Voice(_)) => {}
                },
                () = wake.notified() => self.publish(&scope, &session),
                _ = refresh.tick() => {
                    if let Err(error) = scope.refresh(&session, &self.custody) {
                        tracing::debug!(%error, "joined room checkpoint refresh blocked");
                    }
                    self.publish(&scope, &session);
                }
            }
        }
        scope.close();
        self.set_live(room, None);
        Ok(())
    }

    fn set_live(&self, room: RoomId, live: Option<Arc<RelayCoeditSession>>) {
        if let Ok(mut sessions) = self.sessions.lock()
            && let Some(session) = sessions.get_mut(&room)
        {
            session.live = live;
        }
    }

    /// Room members' presence carries channel governance; nothing else of it
    /// concerns this host.
    fn receive_governance(&self, message: NeighbourMsg) {
        let Some(inbound) = InboundChannelGovernance::from_message(message) else {
            return;
        };
        if let Ok(mut governance) = self.governance.lock() {
            governance.push(inbound);
        }
        self.governance_wake.poke();
    }

    fn publish(&self, scope: &Scope, session: &RelayCoeditSession) {
        let result = match self.keys.get() {
            Some(keys) => scope.publish_renewing(session, &self.custody, &mut |sealed| {
                keys.renew_sealed(scope.room(), sealed)
            }),
            None => scope.publish(session, &self.custody),
        };
        if let Err(error) = result {
            tracing::debug!(%error, "joined room publication remains in durable custody");
        }
    }

    /// Admit a relayed delta; another member's admitted document write then
    /// reaches this device's apps on its route.
    fn admit(&self, input: &Input) {
        let report = match input.admit(&self.custody) {
            Ok(report) => report,
            Err(error) => {
                tracing::warn!(%error, room = %input.room(), "joined room admission denied");
                return;
            }
        };
        if !matches!(
            report.primary(),
            InputDisposition::Accepted(_) | InputDisposition::Duplicate(_)
        ) || input.author() == self.custody.device_signing_key.verifying_key().to_bytes()
        {
            return;
        }
        let NeighbourMsg::AppDocument {
            app_id,
            cluster_id,
            record_id,
            envelope,
            ..
        } = input.message()
        else {
            return;
        };
        let Ok(role) = input.role() else { return };
        if cluster_id != &input.room().to_string() {
            return;
        }
        let author = input.author();
        collab_inbound::deliver(
            &self.channels,
            &self.fanout,
            RemoteDocument {
                app_id: app_id.clone(),
                cluster_id: cluster_id.clone(),
                record_id: record_id.clone(),
                role,
                replayed: input.retained(),
                envelope: (**envelope).clone(),
            },
            |_| format!("peer@{}", hex::encode(author)),
        );
    }

    /// `room`'s connected session, while there is one.
    fn live(&self, room: RoomId) -> Option<Arc<RelayCoeditSession>> {
        self.sessions
            .lock()
            .ok()
            .and_then(|sessions| sessions.get(&room).and_then(|s| s.live.clone()))
    }

    fn wake(&self, room: RoomId) {
        if let Some(wake) = self
            .sessions
            .lock()
            .ok()
            .and_then(|sessions| sessions.get(&room).map(|session| Arc::clone(&session.wake)))
        {
            wake.notify_one();
        }
    }
}

impl OwnerCommitService for Joined {
    fn requires_room(&self, room: &str) -> bool {
        RoomId::parse(room).is_some_and(|room| self.owner.contains_room(room))
    }

    fn commit(&self, request: OwnerCommitRequest) -> CommitFuture {
        let room = RoomId::parse(&request.room);
        let receipt = match (room, self.keys.get()) {
            (Some(room), Some(keys)) => self.owner.commit_document_with_opening(
                request,
                &mut |envelope| keys.open_new_write(room, envelope),
                SystemTime::now(),
            ),
            // Keyless replicas retain the established opaque signed custody.
            // A scoped evaluator must independently require held opening.
            _ => self.owner.commit_document(request, SystemTime::now()),
        };
        if receipt.is_ok()
            && let Some(room) = room
        {
            self.wake(room);
        }
        Box::pin(std::future::ready(receipt))
    }

    fn health(&self) -> Result<(), String> {
        self.owner.health()
    }
}

/// Governance goes to every member of its room, the steward among them, as
/// the room's host fans presence out; it is signed end to end.
impl GovernanceTransport for Joined {
    fn publish(&self, message: NeighbourMsg) -> bool {
        let room = match &message {
            NeighbourMsg::MlsGovernance { room, .. }
            | NeighbourMsg::MlsAccessRequest { room, .. }
            | NeighbourMsg::MlsGovernanceAck { room, .. } => RoomId::parse(room),
            _ => None,
        };
        let live = room.and_then(|room| self.live(room));
        let (Some(live), Ok(bytes)) = (live, postcard::to_allocvec(&message)) else {
            return false;
        };
        live.publish_presence(bytes).is_ok()
    }

    fn take_inbound(&self) -> Vec<InboundChannelGovernance> {
        self.governance
            .lock()
            .map(|mut governance| std::mem::take(&mut *governance))
            .unwrap_or_default()
    }
}

/// The rooms the desktop session enrolled this device in, from its
/// workspace file in an adopted profile.
pub(super) fn desktop_enrolled(profile: &Path) -> io::Result<BTreeSet<RoomId>> {
    #[derive(serde::Deserialize)]
    struct DesktopFile {
        #[serde(default)]
        native_authority: tilo_joined_room::NativeAuthorityStorage,
    }
    match std::fs::read(profile.join(super::workspace::DESKTOP_WORKSPACE)) {
        Ok(bytes) => {
            let file: DesktopFile = serde_json::from_slice(&bytes)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
            Ok(file.native_authority.rooms().collect())
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(BTreeSet::new()),
        Err(error) => Err(error),
    }
}

fn room_topic(room: RoomId) -> Result<u64, String> {
    tilo_relay_protocol::room_workspace_id(&room.to_string()).map_err(|e| e.to_string())
}

#[cfg(test)]
#[path = "joined_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "admission_tests.rs"]
mod admission_tests;
