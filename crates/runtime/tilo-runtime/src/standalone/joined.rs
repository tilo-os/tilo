//! Workspaces this profile joined: enrolled replicas of workspaces a Tilo
//! device or a cloud host governs. Their record is this profile's own file,
//! never the desktop session's; each workspace syncs over one direct link
//! with the host its enrollment pins, which also carries the workspace's
//! channel governance between this device and its participants, as the host
//! forwards what each member's link brings to the others.
use neighbour_wire::NeighbourMsg;
use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    path::Path,
    sync::{Arc, Mutex},
    time::SystemTime,
};
use tilo_action_broker::{
    channel_encryption::{
        ChannelKeyProvider, GovernancePark, GovernanceTransport, InboundChannelGovernance,
    },
    collab_broker::FanoutState,
    collab_core::BrokerChannels,
    collab_inbound::{self, RemoteDocument},
};
use tilo_desktop_model::{Workspace, WorkspaceId, WorkspaceSlot};
use tilo_identity_host::custody::HostIdentity;
use tilo_peer_state::direct::{DirectSync, Heard};
use tilo_peer_state::join::TransportHint;
use tilo_peer_state::owner::{
    NativeWorkspaceOwner,
    session::{Connect, Holder, Input, Link, Linked, Reach, Scope, hold},
};
use tilo_peer_state::owner_commit::{CommitFuture, OwnerCommitRequest, OwnerCommitService};
use tokio::sync::Notify;

#[path = "joined_content.rs"]
mod content;
pub(super) use content::record_content;
#[path = "joined_hosted.rs"]
mod hosted;
pub(super) use hosted::RecordCreation;

/// The record of every joined workspace, in the profile: the file profiles
/// already keep it in.
pub(super) const JOINED_WORKSPACES: &str = "joined-rooms.json";

#[path = "joined_enter.rs"]
pub(in crate::standalone) mod enter;

#[path = "mcp_admission.rs"]
mod mcp_admission;
pub(super) use mcp_admission::{Refusal, SessionAdmission};

pub(super) struct Joined {
    executions: Arc<super::execution::ExecutionPins>,
    owner: NativeWorkspaceOwner,
    custody: HostIdentity,
    endpoint: iroh::Endpoint,
    /// Workspaces this profile must not enroll in, whose writes are refused
    /// here: those the desktop session governs or enrolled this device in.
    refused: BTreeSet<WorkspaceId>,
    channels: Arc<Mutex<BrokerChannels>>,
    fanout: Arc<FanoutState>,
    sessions: Mutex<BTreeMap<WorkspaceId, WorkspaceSession>>,
    /// The profile's record-content service, once the broker runs.
    content: std::sync::OnceLock<Arc<tilo_action_broker::record_content::RouteContent>>,
    /// Channel governance the sessions received, for the channel service.
    governance: Mutex<Vec<InboundChannelGovernance>>,
    /// Wakes the channel service's pump when governance arrives.
    governance_wake: Arc<GovernancePark>,
    /// This device's channel keys, once the channel service runs: the only
    /// renewer of its sealed writes that expire in custody.
    keys: ChannelKeyProvider,
    /// The workspace host this profile runs, once it runs.
    own_host: std::sync::OnceLock<hosted::OwnHost>,
    /// This value, for the futures its commits answer with.
    this: std::sync::Weak<Self>,
}

struct WorkspaceSession {
    /// Wakes the session to send what its owner holds for it.
    wake: Arc<Notify>,
    /// Ticks when the link to the workspace's host comes up or ends: what a
    /// commit waits on before it sends.
    connected: Arc<Notify>,
    /// The link to the workspace's host, while it is up.
    live: Option<Arc<DirectSync>>,
    /// The device key of the host it is linked to, which serves the
    /// workspace's content.
    host: Option<[u8; 32]>,
    /// The devices whose frames this session heard: the workspace's members
    /// reach each other through its host.
    members: std::collections::BTreeSet<[u8; 32]>,
}

/// One joined workspace's session, held for as long as the workspace is.
struct WorkspaceHolder {
    joined: Arc<Joined>,
    /// Wakes the session to send what its owner holds for it.
    wake: Arc<Notify>,
}

impl Holder for WorkspaceHolder {
    fn reach(&self) -> Reach {
        self.joined.reach()
    }

    fn dial_failed(
        &self,
        _via: TransportHint,
        _governor: bool,
        reason: String,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        tracing::debug!(%reason, "a joined workspace's link is unavailable");
        Box::pin(std::future::ready(()))
    }

    fn run(
        &self,
        linked: Linked,
        scope: Arc<Scope>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        Box::pin(self.joined.run_session(linked, scope, &self.wake))
    }
}

impl Joined {
    /// Open the joined workspaces recorded in `profile` and hold a session for
    /// each. `endpoint` is keyed by this device's key, as hosts require.
    pub(super) fn open(
        profile: &Path,
        custody: HostIdentity,
        endpoint: iroh::Endpoint,
        refused: BTreeSet<WorkspaceId>,
        broker: (Arc<Mutex<BrokerChannels>>, Arc<FanoutState>),
    ) -> io::Result<Arc<Self>> {
        let owner = tilo_peer_state::owner::open_file(&profile.join(JOINED_WORKSPACES))
            .map_err(io::Error::other)?;
        let executions = super::execution::ExecutionPins::new(&owner);
        let joined = Arc::new_cyclic(|this| Self {
            executions,
            owner,
            custody,
            endpoint,
            refused,
            channels: broker.0,
            fanout: broker.1,
            sessions: Mutex::new(BTreeMap::new()),
            content: std::sync::OnceLock::new(),
            governance: Mutex::new(Vec::new()),
            governance_wake: Arc::default(),
            keys: ChannelKeyProvider::default(),
            own_host: std::sync::OnceLock::new(),
            this: this.clone(),
        });
        for room in joined.owner.workspaces() {
            joined.hold_session(room);
        }
        Ok(joined)
    }

    /// This device's endpoint, which the profile's other services share.
    pub(super) fn endpoint(&self) -> iroh::Endpoint {
        self.endpoint.clone()
    }

    /// The joined workspaces kept in `profile`, held as the device `custody`
    /// certifies, over `endpoint`: what a profile that serves them alone
    /// starts with, as a member device that takes its workspaces' work does.
    pub(in crate::standalone) fn from_profile(
        profile: &Path,
        custody: HostIdentity,
        endpoint: iroh::Endpoint,
    ) -> io::Result<Arc<Self>> {
        let broker = (
            Arc::new(Mutex::new(BrokerChannels::default())),
            Arc::new(FanoutState::new()),
        );
        let refused = desktop_enrolled(profile)?;
        Self::open(profile, custody, endpoint, refused, broker)
    }

    /// This device's signing key, as its plane signs every request it makes.
    pub(in crate::standalone) fn device_key(&self) -> Result<ed25519_dalek::SigningKey, String> {
        self.custody.signing_key()
    }

    /// The device certificate this profile's custody holds.
    pub(in crate::standalone) fn device_cert(&self) -> &tilo_user_identity::DeviceCert {
        &self.custody.device_cert
    }

    /// The person this device belongs to: whose devices may ask it for work.
    pub(in crate::standalone) fn operator(&self) -> [u8; 32] {
        self.custody.device_cert.user_public_key
    }

    /// How this profile asks the other devices of its workspaces whether they
    /// would run an offloadable action sooner: a plane signed as this device,
    /// at the endpoint they reach it on, reading the joined workspaces as this
    /// profile's owner holds them, and the runtime that carries the asks.
    /// `None` when no runtime runs.
    ///
    /// What it is asked in turn is answered by the host this profile runs
    /// ([`super::joined_hosted`]); this plane only asks.
    pub(super) fn offer_plane(
        &self,
    ) -> Option<(
        tilo_peer_state::actions::ActionPlane,
        iroh::Endpoint,
        tokio::runtime::Handle,
    )> {
        let runtime = tilo_rt::try_app().ok()?.handle().clone();
        let plane = {
            let _entered = runtime.enter();
            tilo_peer_state::actions::ActionPlane::over(
                self.custody.signing_key().ok()?,
                Arc::new(tilo_peer_state::actions::NoActions),
                Arc::new(self.owner.clone()),
            )
        };
        Some((plane, self.endpoint(), runtime))
    }

    /// Redeem `invitation` with the host it names and start syncing the workspace.
    ///
    /// # Errors
    /// As [`Self::enroll`].
    pub(super) async fn join(self: &Arc<Self>, invitation: &str) -> Result<WorkspaceId, String> {
        let workspace = self.enroll(invitation).await?;
        self.hold_session(workspace);
        Ok(workspace)
    }

    /// Redeem `invitation` with the host it names: the workspace is held when
    /// this returns, and synced once its session is held.
    ///
    /// # Errors
    /// The invitation is not for this device, names a workspace this profile
    /// must not enroll in, or the host refuses or cannot be reached; a proof
    /// already sent is kept for an exact retry.
    async fn enroll(&self, invitation: &str) -> Result<WorkspaceId, String> {
        let workspace = self.invited_workspace(invitation)?;
        if self.refused.contains(&workspace) {
            return Err(
                "this device takes part in that workspace through the desktop session".into(),
            );
        }
        let topic = workspace_topic(workspace)?;
        let enrollment = tilo_peer_state::owner::enroll::prepare_and_complete(
            self.join_client(invitation)?,
            self.custody.clone(),
            topic,
            None,
            invitation.to_owned(),
            self.owner.clone(),
        )
        .await?;
        self.owner.install(&enrollment, SystemTime::now())?;
        Ok(workspace)
    }

    /// Every joined workspace, as a workspace its governance describes: who owns
    /// it and every capability in it.
    pub(super) fn workspaces(&self) -> Vec<Workspace> {
        self.owner
            .workspaces()
            .into_iter()
            .filter_map(|workspace| {
                self.current_workspace(workspace)
                    .ok()
                    .map(|(workspace, _)| workspace)
            })
            .collect()
    }

    /// One joined workspace as [`Self::workspaces`] describes it, built alone:
    /// asking about one workspace never rebuilds every other joined workspace.
    pub(super) fn workspace(&self, workspace: WorkspaceId) -> Option<Workspace> {
        if !self.owner.workspaces().contains(&workspace) {
            return None;
        }
        self.current_workspace(workspace)
            .ok()
            .map(|(workspace, _)| workspace)
    }

    /// This device as `workspace` knows it, for the workspace's Files
    /// endpoint: the certificate the workspace holds for this device's key.
    ///
    /// # Errors
    /// `workspace` is no workspace id, or the certificate is not valid now.
    pub(super) fn files_credentials(
        &self,
        workspace: &str,
    ) -> io::Result<tilo_fs::identity::PeerCredentials> {
        let workspace =
            WorkspaceId::parse(workspace).ok_or_else(|| io::Error::other("not a workspace id"))?;
        let (cert, _) = self
            .custody
            .material_for(workspace_topic(workspace).map_err(io::Error::other)?);
        tilo_fs::identity::PeerCredentials::certified(
            cert,
            self.custody.signing_key().map_err(io::Error::other)?,
            tilo_clock::unix_now_ns(),
        )
        .map_err(io::Error::other)
    }

    /// Whether this profile has joined any workspace.
    pub(super) fn is_empty(&self) -> bool {
        self.owner.workspaces().is_empty()
    }

    // Use the same current custody check as an enrolled relay session. Merely
    // retaining a peer or its device key cannot authenticate a selected root.
    fn current_workspace(&self, workspace: WorkspaceId) -> Result<(Workspace, String), String> {
        let (_, cert, continuity) =
            Connect::new(self.owner.clone(), workspace)?.material(&self.custody)?;
        self.owner.with_current(workspace, |view| {
            if view.continuity() != continuity || view.device() != &cert {
                return Err("workspace identity changed while selecting the audience".into());
            }
            let principal = view
                .peer()
                .permissions(cert.device_public_key, SystemTime::now())
                .map_err(|error| error.to_string())?
                .person;
            let document = view.peer().admitted_workspace().ok_or("no admitted room")?;
            let mut workspace = Workspace::in_workspace(
                WorkspaceSlot(view.workspace_id()),
                workspace,
                String::new(),
            );
            tilo_peer_state::workspace::adopt_workspace(&mut workspace, document, &[]);
            workspace.sandbox = view.peer().governance().governed_sandbox()?;
            Ok((workspace, principal))
        })
    }

    /// Select only a workspace this exact profile principal and certified device
    /// currently take part in. Reading authority never joins or requests keys.
    pub(super) fn action_workspace(&self, actor: &str, workspace: &str) -> Result<String, String> {
        let workspace = WorkspaceId::parse(workspace).ok_or("invalid MCP workspace")?;
        let (_, principal) = self.current_workspace(workspace)?;
        if actor != principal {
            return Err(
                "the selected profile actor is not this workspace's admitted identity".into(),
            );
        }
        Ok(workspace.to_string())
    }

    /// The slot the channel service fills with this device's channel keys.
    pub(super) fn channel_keys(&self) -> ChannelKeyProvider {
        self.keys.clone()
    }

    /// What the sessions poke when governance arrives for the channel
    /// service, whose pump otherwise sleeps while no channel is live.
    pub(super) fn governance_wake(&self) -> Arc<GovernancePark> {
        Arc::clone(&self.governance_wake)
    }

    /// The member devices `workspace`'s governance lets read `channel` at `now`
    /// (Unix seconds): the projection the channel's group follows.
    pub(super) fn channel_readers(
        &self,
        workspace: WorkspaceId,
        channel: collab_types::ChannelId,
        route: Option<&collab_types::ChannelRoute>,
        now: u64,
    ) -> Option<std::collections::BTreeSet<collab_types::Ed25519PublicKey>> {
        let at = std::time::SystemTime::UNIX_EPOCH
            .checked_add(tilo_action_broker::channel_encryption::policy_instant(now))
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        let readers = self
            .owner
            .with_current(workspace, |view| view.channel_readers(at, channel, route))
            // Governance that cannot be read says nothing of who reads.
            .ok()
            .flatten()?;
        Some(
            readers
                .into_iter()
                .map(collab_types::Ed25519PublicKey::from_bytes)
                .collect(),
        )
    }

    /// The scope `route`'s record seals under, as `workspace`'s governance
    /// decides it at `now` (Unix seconds), or `None` while governance cannot
    /// be read: a guessed scope could hand a device keys its record's policy
    /// withholds.
    pub(super) fn unit_scope(
        &self,
        workspace: WorkspaceId,
        route: &collab_types::ChannelRoute,
        now: u64,
    ) -> Option<collab_types::ChannelId> {
        let at = std::time::SystemTime::UNIX_EPOCH
            .checked_add(tilo_action_broker::channel_encryption::policy_instant(now))
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        self.owner
            .with_current(workspace, |view| view.unit_scope(at, route))
            .ok()
    }

    fn invited_workspace(&self, invitation: &str) -> Result<WorkspaceId, String> {
        let now = tilo_clock::try_unix_now_ns().ok_or("the clock is before the Unix epoch")?;
        let join = tilo_peer_state::join::decode_and_verify_invitation_text(
            invitation,
            Some(self.custody.device_cert.user_public_key),
            &tilo_user_identity::RevocationList::new(),
            now,
        )?;
        WorkspaceId::parse(&join.grant().room)
            .ok_or_else(|| "invitation names no valid workspace".into())
    }

    pub(super) fn hold_session(self: &Arc<Self>, workspace: WorkspaceId) {
        let Ok(mut sessions) = self.sessions.lock() else {
            return;
        };
        if sessions.contains_key(&workspace) {
            return;
        }
        let wake = Arc::new(Notify::new());
        sessions.insert(
            workspace,
            WorkspaceSession {
                wake: Arc::clone(&wake),
                connected: Arc::new(Notify::new()),
                live: None,
                host: None,
                members: std::collections::BTreeSet::new(),
            },
        );
        drop(sessions);
        let Ok(connect) = Connect::new(self.owner.clone(), workspace) else {
            return;
        };
        let identity = Some(self.custody.clone());
        let holder = WorkspaceHolder {
            joined: Arc::clone(self),
            wake,
        };
        tilo_rt::app().spawn(async move {
            hold(connect, identity, &holder, &Link::default()).await;
        });
    }

    async fn run_session(&self, linked: Linked, scope: Arc<Scope>, wake: &Notify) {
        let workspace = scope.workspace();
        let link = Arc::new(linked.link.clone());
        let host = scope.host().host.device_public_key;
        // Subscribed before the link runs, so nothing it brings in is missed.
        let mut heard = link.heard();
        // The link is the workspace's to send on once it caught up with its
        // host's history: a frame sent before reaches no one, and what this
        // device holds for the workspace waits for it.
        let mut caught_up = link.caught_up();
        let mut live = false;
        let mut running = Box::pin(linked.run());
        let mut sending = tilo_session_client::custody::Sending::default();
        let ended = loop {
            tokio::select! {
                ended = &mut running => break ended,
                caught = caught_up.wait_for(|up| *up), if !live => {
                    if caught.is_err() {
                        break Err("the link closed".into());
                    }
                    live = true;
                    self.set_live(workspace, Some((Arc::clone(&link), host)));
                    self.publish(&scope, &link, &mut sending);
                }
                frame = heard.recv() => match frame {
                    Ok(heard) => {
                        self.take(&scope, heard);
                        self.publish(&scope, &link, &mut sending);
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        break Err("the session fell behind the workspace's frames".into());
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        break Err("the link closed".into());
                    }
                },
                () = wake.notified() => self.publish(&scope, &link, &mut sending),
            }
        };
        if let Err(reason) = ended {
            tracing::debug!(%workspace, %reason, "a joined workspace's link ended");
        }
        self.set_live(workspace, None);
    }

    /// A frame the link brought this profile: what the workspace's members say
    /// to each other beside their records, then the record itself.
    fn take(&self, scope: &Arc<Scope>, heard: Heard) {
        let workspace = scope.workspace();
        let (author, message) = (heard.author, heard.message.clone());
        // The workspace's retained history reaches this link too: a frame out
        // of it is its author's record, not word that its author is here now,
        // and neither an ask nor an answer to give.
        if !heard.retained {
            self.membership(workspace, author);
            // Content and channel governance are between the workspace's
            // devices: they go to the services that keep them, never into its
            // records.
            if self.content_wanted(workspace, &message) {
                return;
            }
            if matches!(
                message,
                NeighbourMsg::RecordContentPiece { .. } | NeighbourMsg::RecordContentLacks { .. }
            ) {
                self.content_arrived(workspace, author, &message);
                return;
            }
        }
        self.receive_governance(message);
        let input = Input::heard(Arc::clone(scope), heard);
        match input.admit() {
            Ok(report) => {
                // Another member's write reaches this profile's apps on its
                // route, and so does every held write it brought what that
                // write waited for.
                for write in input.app_writes(&report) {
                    let author = write.envelope.author.as_bytes().to_owned();
                    collab_inbound::deliver(
                        &self.channels,
                        &self.fanout,
                        RemoteDocument {
                            app_id: write.app_id,
                            cluster_id: write.cluster_id,
                            record_id: write.record_id,
                            author: write.author,
                            replayed: write.replayed,
                            envelope: write.envelope,
                        },
                        |_| format!("peer@{}", hex::encode(author)),
                    );
                }
            }
            Err(reason) => {
                tracing::debug!(%reason, %workspace, "a joined workspace's frame was not taken in");
            }
        }
    }

    /// `workspace`'s link connected to its host, or ended. The workspace's
    /// content follows: what is wanted is asked of a host that connected, and
    /// waits while there is none.
    fn set_live(&self, workspace: WorkspaceId, live: Option<(Arc<DirectSync>, [u8; 32])>) {
        let mut connected = None;
        if let Ok(mut sessions) = self.sessions.lock()
            && let Some(session) = sessions.get_mut(&workspace)
        {
            (session.live, session.host) = live.unzip();
            session.members.clear();
            connected = Some(Arc::clone(&session.connected));
        }
        if let Some(connected) = connected {
            connected.notify_waiters();
        }
        if let Some(content) = self.content.get().cloned() {
            tilo_rt::app().spawn_blocking(move || content.sources_changed());
        }
    }

    /// Ticks when `workspace`'s link to its host comes up or ends.
    fn connected(&self, workspace: WorkspaceId) -> Result<Arc<Notify>, String> {
        self.sessions
            .lock()
            .map_err(|_| "the joined workspaces closed".to_owned())?
            .get(&workspace)
            .map(|session| Arc::clone(&session.connected))
            .ok_or_else(|| "this profile holds no session for the workspace".to_owned())
    }

    /// A device of `workspace` this session heard a frame from, other than
    /// this profile's own and the host it is linked to, which its link
    /// already names.
    fn membership(&self, workspace: WorkspaceId, device: [u8; 32]) {
        if device == self.custody.device_public_key() {
            return;
        }
        if let Ok(mut sessions) = self.sessions.lock()
            && let Some(session) = sessions.get_mut(&workspace)
            && session.host != Some(device)
            && session.members.insert(device)
            && let Some(content) = self.content.get().cloned()
        {
            drop(sessions);
            tilo_rt::app().spawn_blocking(move || content.sources_changed());
        }
    }

    /// The devices this session heard `workspace`'s frames from.
    fn live_members(&self, workspace: WorkspaceId) -> Vec<[u8; 32]> {
        self.sessions
            .lock()
            .ok()
            .and_then(|sessions| {
                sessions
                    .get(&workspace)
                    .filter(|s| s.live.is_some())
                    .map(|s| s.members.iter().copied().collect())
            })
            .unwrap_or_default()
    }

    /// Hand the channel governance a workspace host running on this device
    /// took in to this device's channel service, which settles the groups from
    /// it as it does from what its own sessions hear.
    pub(super) fn take_channel_governance(&self, messages: Vec<NeighbourMsg>) {
        for message in messages {
            self.receive_governance(message);
        }
    }

    /// The device key of the host `workspace`'s session is connected to.
    fn live_host(&self, workspace: WorkspaceId) -> Option<[u8; 32]> {
        self.sessions.lock().ok().and_then(|sessions| {
            sessions
                .get(&workspace)
                .and_then(|s| s.live.as_ref().and(s.host))
        })
    }

    /// Workspace members' presence carries channel governance; nothing else of it
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

    /// Send `workspace`'s own writes, and what its sealed records lack, to its
    /// host: the link carries them to every member.
    fn publish(
        &self,
        scope: &Scope,
        link: &DirectSync,
        sending: &mut tilo_session_client::custody::Sending,
    ) {
        let workspace = scope.workspace();
        // What the workspace answered for leaves custody before what it still
        // holds is renewed and sent.
        if let Err(error) = self.owner.retire_outgoing(workspace) {
            tracing::warn!(%error, "joined workspace publication retirement not committed");
        }
        let sent = scope.current(&self.custody).and_then(|()| {
            if let Some(keys) = self.keys.get() {
                self.fold_and_renew(workspace, &keys)?;
            }
            sending.drive(
                &self.owner,
                workspace,
                |view| scope.authorize(view, &self.custody),
                |write| {
                    postcard::from_bytes(write.bytes)
                        .map_err(|error| error.to_string())
                        .and_then(|message| link.send(message))
                        .is_ok()
                },
            )
        });
        if let Err(error) = sent {
            tracing::debug!(%error, "joined workspace publication remains in durable custody");
        }
    }

    /// A sealed record's history this device can read is this device's to
    /// fold: it writes its own checkpoint where the workspace's host used to.
    /// Then its sealed writes that expired in custody are sealed again.
    fn fold_and_renew(
        &self,
        workspace: WorkspaceId,
        keys: &tilo_action_broker::channel_encryption::ChannelKeyAccess,
    ) -> Result<(), String> {
        let key = self.custody.signing_key()?;
        match self.owner.compact_outgoing(
            workspace,
            &key,
            &mut |write| keys.open_history_write(workspace, write),
            &mut |app, record, snapshot| keys.seal_checkpoint(workspace, app, record, snapshot),
            SystemTime::now(),
        ) {
            Ok(0) => {}
            Ok(count) => tracing::debug!(count, "joined workspace sealed histories compacted"),
            Err(error) => {
                tracing::debug!(%error, "joined workspace sealed history stays uncompacted");
            }
        }
        if let Err(error) = self.owner.renew_outgoing(
            workspace,
            &key,
            &mut |sealed| keys.renew_sealed(workspace, sealed),
            SystemTime::now(),
        ) {
            tracing::warn!(%workspace, %error, "native sealed write renewal blocked");
        }
        Ok(())
    }

    /// `workspace`'s connected link, while there is one.
    fn live(&self, workspace: WorkspaceId) -> Option<Arc<DirectSync>> {
        self.sessions
            .lock()
            .ok()
            .and_then(|sessions| sessions.get(&workspace).and_then(|s| s.live.clone()))
    }

    fn wake(&self, workspace: WorkspaceId) {
        if let Some(wake) = self.sessions.lock().ok().and_then(|sessions| {
            sessions
                .get(&workspace)
                .map(|session| Arc::clone(&session.wake))
        }) {
            wake.notify_one();
        }
    }
}

impl OwnerCommitService for Joined {
    /// Every workspace this profile joined, and the workspaces the desktop session
    /// governs here, whose writes are refused.
    fn requires_workspace(&self, room: &str) -> bool {
        WorkspaceId::parse(room)
            .is_some_and(|room| self.owner.contains_workspace(room) || self.refused.contains(&room))
    }

    /// A workspace hosted here keeps its effect receipts in the profile's own
    /// workspace record.
    fn records_effects(&self, room: &str) -> bool {
        WorkspaceId::parse(room).is_some_and(|room| {
            self.owner.contains_workspace(room) && !self.hosted_here(room)
                || self.refused.contains(&room)
        })
    }

    fn commit(&self, request: OwnerCommitRequest) -> CommitFuture {
        self.commit_with_record_kind(request, None)
    }

    fn commit_with_record_kind(
        &self,
        request: OwnerCommitRequest,
        record_kind: Option<String>,
    ) -> CommitFuture {
        let workspace = WorkspaceId::parse(&request.workspace);
        if workspace.is_some_and(|workspace| self.refused.contains(&workspace)) {
            return Box::pin(std::future::ready(Err(
                "the desktop session's workspace is read only in the runtime".into(),
            )));
        }
        if let Some(workspace) = workspace.filter(|workspace| self.hosted_here(*workspace)) {
            let Some(this) = self.this.upgrade() else {
                return Box::pin(std::future::ready(Err(
                    "the joined workspaces closed".into()
                )));
            };
            return Box::pin(
                async move { this.commit_hosted(workspace, request, record_kind).await },
            );
        }
        // Drafted again past its record's floor when this device's local writers signed out of
        // the order the owner takes them, for this device's custody to sign.
        let (owner, signer, this) = (
            self.owner.clone(),
            Arc::clone(&self.custody.signer),
            self.this.clone(),
        );
        Box::pin(async move {
            let (_, receipt) = owner
                .commit_local_document(request, signer.as_ref(), SystemTime::now())
                .await?;
            if let Some(workspace) = workspace
                && let Some(this) = this.upgrade()
            {
                this.wake(workspace);
            }
            Ok(receipt)
        })
    }

    fn health(&self) -> Result<(), String> {
        self.owner.health()
    }
}

/// Governance goes to every member of its workspace, as the workspace's host fans
/// presence out; it is signed end to end.
impl GovernanceTransport for Joined {
    fn publish(&self, message: NeighbourMsg) -> bool {
        let workspace = message.channel_workspace().and_then(WorkspaceId::parse);
        let Some(live) = workspace.and_then(|workspace| self.live(workspace)) else {
            return false;
        };
        live.send(message).is_ok()
    }

    fn take_inbound(&self) -> Vec<InboundChannelGovernance> {
        self.governance
            .lock()
            .map(|mut governance| std::mem::take(&mut *governance))
            .unwrap_or_default()
    }
}

/// The workspaces the desktop session enrolled this device in, from its owner's
/// record beside its workspace file in an adopted profile.
pub(super) fn desktop_enrolled(profile: &Path) -> io::Result<BTreeSet<WorkspaceId>> {
    let record =
        tilo_peer_state::owner::owner_file_of(&profile.join(super::workspace::DESKTOP_WORKSPACE));
    tilo_peer_state::owner::read_file(&record)
        .map(|storage| storage.workspaces().collect())
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

fn workspace_topic(workspace: WorkspaceId) -> Result<u64, String> {
    tilo_relay_protocol::room_workspace_id(&workspace.to_string()).map_err(|e| e.to_string())
}

#[cfg(test)]
#[path = "joined_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "mcp_admission_tests.rs"]
mod mcp_admission_tests;

#[cfg(test)]
impl Joined {
    /// Read this profile's custody from `paths` from now on, before any run
    /// is admitted: the tests that make one read of it fail compare the same
    /// custody, read and unread.
    ///
    /// The fixture is the only holder here; every run is admitted after.
    pub(in crate::standalone) fn install_custody_paths(
        &self,
        paths: tilo_user_identity::persist::IdentityPaths,
    ) {
        let custody = &raw const self.custody;
        // SAFETY: no run has been admitted yet, so nothing reads this store
        // concurrently; each admission afterwards pins what it reads.
        unsafe {
            (*(custody.cast_mut())).custody =
                std::sync::Arc::new(tilo_identity_host::custody::files::Files::new(paths));
        }
    }
}

#[cfg(test)]
#[path = "admission_tests.rs"]
pub(super) mod admission_tests;
