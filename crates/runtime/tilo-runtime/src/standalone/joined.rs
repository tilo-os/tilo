//! Rooms this profile joined: enrolled replicas of rooms a Tilo device or a
//! cloud host governs. Their record is this profile's own file, never the
//! desktop session's; each room syncs over one relay session with the host
//! its enrollment pins. The room's governor applies a foreign device's limits
//! to this device there, as it does to every member.
use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime},
};
use tilo_action_broker::{
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
use tilo_relay::{Inbound, RelayCoeditSession, workspace_join::WorkspaceJoinClient};
use tokio::sync::Notify;

/// The record of every joined room, in the profile.
pub(super) const JOINED_ROOMS: &str = "joined-rooms.json";
/// How often a live session asks its host for a fresh checkpoint.
const REFRESH_INTERVAL: Duration = Duration::from_secs(30);
const REDIAL_LONGEST: Duration = Duration::from_secs(60);

pub(super) struct Joined {
    owner: NativeWorkspaceOwner,
    custody: HostIdentity,
    endpoint: iroh::Endpoint,
    /// Rooms this profile must not enroll in: its own, and those the
    /// desktop session already enrolled this device in.
    refused: BTreeSet<RoomId>,
    channels: Arc<Mutex<BrokerChannels>>,
    fanout: Arc<FanoutState>,
    /// Wakes a room's session to send what its owner holds for it.
    sessions: Mutex<BTreeMap<RoomId, Arc<Notify>>>,
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
        let joined = Arc::new(Self {
            owner,
            custody,
            endpoint,
            refused,
            channels: broker.0,
            fanout: broker.1,
            sessions: Mutex::new(BTreeMap::new()),
        });
        for room in joined.owner.rooms() {
            joined.hold_session(room);
        }
        Ok(joined)
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
            .filter_map(|room| {
                self.owner
                    .with_current(room, |view| {
                        let document = view.peer().admitted_room().ok_or("no admitted room")?;
                        let (name, owner, capabilities) =
                            tilo_peer_state::room::extract_governed_fields(document);
                        let mut workspace =
                            Workspace::new(WorkspaceId(view.workspace()), name.unwrap_or_default());
                        workspace.room = room;
                        workspace.owner = owner;
                        workspace.capabilities = capabilities;
                        Ok(workspace)
                    })
                    .ok()
            })
            .collect()
    }

    fn invited_room(&self, invitation: &str) -> Result<RoomId, String> {
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_nanos();
        let join = tilo_peer_state::join::decode_and_verify_invitation_text(
            invitation,
            Some(self.custody.device_cert.user_public_key),
            &tilo_user_identity::RevocationList::new(),
            u64::try_from(now).map_err(|e| e.to_string())?,
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
        sessions.insert(room, Arc::clone(&wake));
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
        let session = session::dial(
            self.endpoint.clone(),
            room_topic(connect.room())?,
            &hint,
            device.clone(),
            self.custody.device_signing_key.clone(),
            revocations,
        )
        .await?;
        let scope = Arc::new(Scope::new(connect, hint, device, continuity));
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
                    Some(Inbound::Presence { .. } | Inbound::RemoteAction { .. }) => {}
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
        Ok(())
    }

    fn publish(&self, scope: &Scope, session: &RelayCoeditSession) {
        if let Err(error) = scope.publish(session, &self.custody) {
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
                envelope: (**envelope).clone(),
            },
            |_| format!("peer@{}", hex::encode(author)),
        );
    }

    fn wake(&self, room: RoomId) {
        if let Some(wake) = self
            .sessions
            .lock()
            .ok()
            .and_then(|s| s.get(&room).cloned())
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
        let receipt = self.owner.commit_document(request, SystemTime::now());
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
