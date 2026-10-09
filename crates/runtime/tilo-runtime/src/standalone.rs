//! Services for a runtime profile without the Tilo desktop: the shared
//! action broker and identity service over the profile's own workspace.
//!
//! This is what the runtime serves on stock macOS or a foreign Linux
//! desktop. The desktop session hosts the same brokers with its native
//! adapters (launcher, governance, durable owner, peer network). Here the
//! profile launches its installed apps through the platform launcher
//! ([`crate::apps`]), records the grants people give apps for other apps'
//! actions in its own workspace, owns its room's documents durably
//! ([`owner`]), local to this machine, joins rooms a Tilo device or cloud
//! host governs ([`joined`]) and serves route keys for both
//! ([`channels`]); every other host service is absent,
//! and the broker answers as it does for any host that does not provide one.
//! What this host keeps durably is in the profile: the room, grants, effect
//! and task records in its own workspace file, the invocation journal and
//! collaboration history.

mod channels;
mod execution;
mod files;
mod joined;
mod owner;
mod rooms;
mod workspace;

use crate::{
    BLOB_SOCKET, COLLAB_SOCKET, IDENTITY_SOCKET, Joiner, RuntimeServices, apps, profile, unix,
};
use joined::Joined;
use owner::{AdoptedRoom, RecordCreation, RoomOwner};
use rooms::{Owners, Rooms};
use std::{
    io,
    path::{Path, PathBuf},
    process::{Child, Command},
    sync::{Arc, Mutex},
};
use tilo_action_broker::{
    collab_broker::{AppActionBroker, BrokerHandle, FanoutState},
    collab_core::BrokerChannels,
    collab_history::CollabHistoryStore,
    collab_outbound::PeerPublisher,
    services::{HostServices, LauncherServices, LegacyWorkspaceProjection, WorkspaceAuthority},
};
use tilo_actions::remote::{
    ActionHost, ActionHostError, Caller, CataloguePage, CatalogueQuery, InvocationReceipt,
    InvocationState, InvokeRequest, Pending,
};
use tilo_desktop_model::RoomId;
use tilo_identity_host::{
    broker::{IdentityBrokerHandle, WorkspaceMembership},
    provisioning::IdentityStorage,
};
use tilo_peer_state::owner_commit::OwnerCommitService;
use workspace::{ProfileGrants, ProfileWorkspace};

/// Durable collaboration history, where the desktop session keeps it under
/// the profile's `XDG_STATE_HOME`.
const COLLABORATION_HISTORY: &str = "state/tilo/collaboration/history";

/// A standalone service session over one profile. Drop applications first.
pub struct Standalone {
    // Retired before broker fields: retained original custody cannot survive
    // replacement of the runtime that supplied its policy time.
    policy_origin: tilo_action_broker::peer_actions::PolicyOrigin,
    broker: BrokerHandle,
    joined: Arc<Joined>,
    _channels: channels::Channels,
    _identity: IdentityBrokerHandle,
    room: String,
    actor: String,
    identity_socket: PathBuf,
    blob_socket: PathBuf,
    /// Before the media host, whose content store it uses.
    _files: Option<files::WorkspaceFiles>,
    _media: profile::MediaHost,
    _lock: advisory_lock::FileLock,
}

impl Drop for Standalone {
    fn drop(&mut self) {
        self.policy_origin.invalidate();
    }
}

impl Standalone {
    fn start(root: &Path, stage: &mut dyn FnMut(&str), dev: bool) -> io::Result<Self> {
        stage("locking the profile");
        let lock = profile::lock(root)?;
        stage("provisioning the collaboration identity");
        let storage = IdentityStorage::for_session();
        let device = provision_device(storage)?;
        stage("loading the workspace");
        let workspace = ProfileWorkspace::open(root)?;
        let room_id = workspace.room();
        let identity = tilo_identity_host::registry::selected_identity_profile(&room_id)?
            .ok_or_else(|| io::Error::other("runtime workspace has no selected identity"))?;
        let actor = identity.principal_id();
        stage("saving the workspace");
        workspace.adopt(&actor)?;
        let workspace = Arc::new(workspace);
        let apps = Arc::new(apps::Apps::from_env(root, &room_id));
        serve_device_grants(root, &apps);
        let channels = Arc::new(Mutex::new(channels(root)));
        let fanout = Arc::new(FanoutState::new());
        stage("opening the joined rooms and channel keys");
        let (joined, channel_keys) = joined_rooms_and_keys(
            root,
            &room_id,
            storage,
            (Arc::clone(&channels), Arc::clone(&fanout)),
        )?;
        stage("opening the room owner");
        let owner = room_owner(
            root,
            &room_id,
            &workspace,
            &apps,
            dev,
            channel_keys.keys.clone(),
        )?;
        let rooms = Arc::new(Rooms {
            profile: Arc::clone(&workspace),
            joined: Arc::clone(&joined),
        });
        let identity_socket = root.join(IDENTITY_SOCKET);
        stage("starting the identity service");
        let identity_broker = tilo_identity_host::broker::spawn(
            identity_socket.clone(),
            Arc::clone(&rooms) as Arc<dyn WorkspaceMembership>,
            Arc::new(channel_keys.keys.clone()),
        )?;
        stage("starting the collaboration broker");
        let (policy_origin, policy_time) = execution_policy_clock()?;
        let owners = Arc::new(Owners {
            own: owner,
            joined: Arc::clone(&joined),
        });
        let broker = tilo_action_broker::collab_broker::spawn_with_services(
            root.join(COLLAB_SOCKET),
            Some(rooms as Arc<dyn WorkspaceAuthority>),
            PeerPublisher::with_optional_owner_commit(None, owners),
            None,
            HostServices {
                native_certificates: Some(Arc::new(Certificates {
                    room: room_id.clone(),
                    device,
                    policy_time,
                    joined: Arc::downgrade(&joined),
                })),
                launchable_catalogue: Some(root.join("state/launchable-actions.json")),
                launcher: Some(Arc::clone(&apps) as Arc<dyn LauncherServices>),
                app_action_access: Some(apps.access_resolver()),
                app_action_grants: Some(Arc::new(ProfileGrants {
                    workspace: Arc::clone(&workspace),
                    apps,
                })),
                legacy_workspace: Some(Arc::clone(&workspace) as Arc<dyn LegacyWorkspaceProjection>),
                record_content: Some(joined::record_content(&joined, &root.join(BLOB_SOCKET))),
                // The service the identity service answers key requests
                // from: endpoints ask again when it says a channel changed.
                channel_keys: Some(channel_keys.keys.clone()),
                ..HostServices::default()
            },
            channels,
            fanout,
        )?;
        let blob_socket = root.join(BLOB_SOCKET);
        stage("opening the content store");
        let media = profile::MediaHost::start(&root.join("content"), &blob_socket)?;
        let author = files::Author {
            person: identity.user_public_key,
            device,
        };
        let files = workspace_files(root, &workspace, &media, joined.endpoint(), &author, stage);
        Ok(Self {
            policy_origin,
            broker,
            joined,
            _channels: channel_keys,
            _identity: identity_broker,
            room: room_id,
            actor,
            identity_socket,
            blob_socket,
            _files: files,
            _media: media,
            _lock: lock,
        })
    }
}

fn execution_policy_clock() -> io::Result<(
    tilo_action_broker::peer_actions::PolicyOrigin,
    tilo_action_broker::peer_actions::PolicyTime,
)> {
    let origin = tilo_action_broker::peer_actions::OwnerPolicyClock::new()
        .map_err(io::Error::other)?
        .new_origin();
    let time = origin.capture().map_err(io::Error::other)?;
    Ok((origin, time))
}

impl RuntimeServices for Standalone {
    type ActionHost = RuntimeActionHost;

    fn start_reporting(root: &Path, stage: &mut dyn FnMut(&str)) -> io::Result<Self> {
        Self::start(root, stage, false)
    }

    fn start_in_dev_mode(root: &Path, stage: &mut dyn FnMut(&str)) -> io::Result<Self> {
        Self::start(root, stage, true)
    }

    fn room(&self) -> &str {
        &self.room
    }

    fn mcp_room(&self, requested: Option<&str>) -> io::Result<String> {
        let room = requested.unwrap_or(&self.room);
        if room == self.room {
            return Ok(self.room.clone());
        }
        self.joined
            .action_room(&self.actor, room)
            .map_err(|error| io::Error::new(io::ErrorKind::PermissionDenied, error))
    }

    fn actor(&self) -> &str {
        &self.actor
    }

    fn action_host(&self) -> io::Result<RuntimeActionHost> {
        self.broker
            .app_action_broker()
            .map(|broker| RuntimeActionHost {
                broker,
                room: self.room.clone(),
                joined: Arc::clone(&self.joined),
                session: None,
            })
            .ok_or_else(|| io::Error::other("runtime action broker is unavailable"))
    }

    fn mcp_session_factory(
        &self,
        requested: &str,
    ) -> io::Result<crate::mcp::SessionFactory<RuntimeActionHost>> {
        let room = self.mcp_room(Some(requested))?;
        let actor = self.actor.clone();
        let host = self.action_host()?;
        Ok(host.session_factory(
            room,
            actor,
            self.policy_origin.capture().map_err(io::Error::other)?,
        ))
    }

    fn joiner(&self) -> Option<Joiner> {
        let joined = Arc::clone(&self.joined);
        Some(Arc::new(move |invitation: &str| {
            let joined = Arc::clone(&joined);
            let invitation = invitation.to_owned();
            tilo_rt::app()
                .handle()
                .block_on(async move { joined.join(&invitation).await })
                .map(|room| room.to_string())
                .map_err(io::Error::other)
        }))
    }

    fn launch(&self, app_id: &str, command: Command) -> io::Result<Child> {
        unix::launch(
            command,
            app_id,
            &self.room,
            &unix::ServiceSockets {
                collab: self.broker.capnp_socket(),
                identity: &self.identity_socket,
                blobs: &self.blob_socket,
            },
        )
    }

    fn media(&self) -> Option<Arc<dyn crate::MediaReader>> {
        Some(Arc::new(profile::BlobMedia::new(
            &self.blob_socket,
            &self.room,
        )))
    }
}

/// The broker's action door as one more local caller, standing for no app
/// connection. Its callers act from this foreign device.
#[derive(Clone)]
pub struct RuntimeActionHost {
    broker: AppActionBroker,
    room: String,
    joined: Arc<Joined>,
    // Present only on the per-execution MCP factory path; no fallback after
    // qualification fails. Existing local OS search keeps its original door.
    session: Option<Arc<AdmittedRuntimeSession>>,
}

struct AdmittedRuntimeSession {
    policy_time: tilo_action_broker::peer_actions::PolicyTime,
    host: Arc<dyn ActionHost + Send + Sync>,
    admission: Arc<joined::SessionAdmission>,
    /// The agent the person's device handed this run to.
    agent: String,
}
impl AdmittedRuntimeSession {
    /// The run's caller as the broker admits it: the agent, for the person.
    fn acting(&self, caller: &Caller) -> Caller {
        caller.clone().delegated_to(self.agent.clone())
    }

    fn check(&self, caller: &Caller, joined: &Joined) -> Result<(), ActionHostError> {
        let now = self
            .policy_time
            .now_ns()
            .map_err(|_| ActionHostError::NotPermitted)?;
        tilo_user_identity::verify_device_cert_at(self.admission.certificate(), now)
            .map_err(|_| ActionHostError::NotPermitted)?;
        if caller != self.admission.caller() || self.admission.check(joined).is_err() {
            return Err(ActionHostError::NotPermitted);
        }
        Ok(())
    }

    fn guarded<T: Send + 'static>(
        self: &Arc<Self>,
        joined: Arc<Joined>,
        caller: Caller,
        answer: Pending<Result<T, ActionHostError>>,
    ) -> Pending<Result<T, ActionHostError>> {
        let session = Arc::clone(self);
        let (complete, pending) = tilo_actions::remote::pending();
        answer.then(move |result| {
            let result = session.check(&caller, &joined).and_then(|()| {
                result.unwrap_or_else(|| {
                    Err(ActionHostError::Unavailable {
                        reason: "action door closed".into(),
                    })
                })
            });
            complete.complete(result);
        });
        pending
    }
}

impl RuntimeActionHost {
    fn session_factory(
        self,
        room: String,
        actor: String,
        policy_time: tilo_action_broker::peer_actions::PolicyTime,
    ) -> crate::mcp::SessionFactory<Self> {
        let host = self;
        Arc::new(move || {
            let admission = host
                .joined
                .admit_mcp_execution(&room, &host.room, &actor)
                .map_err(io::Error::other)?;
            let caller = admission.caller().clone();
            // The MCP client is an agent the person's device hands this
            // session to; the admission stays the person's.
            let agent = crate::mcp::agent_id(&caller);
            let qualified = host
                .broker
                .certified_local_agent_host(&caller, admission.presentation(), &agent)
                .map_err(io::Error::other)?;
            admission.check(&host.joined).map_err(io::Error::other)?;
            let session_host = RuntimeActionHost {
                broker: host.broker.clone(),
                room: host.room.clone(),
                joined: host.joined.clone(),
                session: Some(Arc::new(AdmittedRuntimeSession {
                    policy_time: policy_time.clone(),
                    host: qualified,
                    admission,
                    agent,
                })),
            };
            crate::mcp::McpServer::admitted(session_host, caller)
        })
    }

    fn admitted(&self, caller: &Caller) -> bool {
        let Some(room) = caller.room().filter(|room| *room != self.room) else {
            return true;
        };
        let tilo_actions::remote::CallerPrincipal::Person(actor) = caller.principal() else {
            return false;
        };
        self.joined.action_room(actor, room).is_ok()
    }
}

// These existing alias forms may eventually dispatch into a published app.
// Keep original certified custody through the broker's deferred launch path.
fn certified_provider(provider: &str) -> bool {
    provider.parse::<u64>().is_ok()
        || tilo_actions::remote::application_provider(provider).is_some()
        || provider.starts_with("launch:")
}

fn on_this_device(caller: &Caller) -> Caller {
    caller.clone().from_foreign_device(true)
}

impl ActionHost for RuntimeActionHost {
    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionHostError>> {
        if let Some(session) = &self.session {
            if let Err(error) = session.check(caller, &self.joined) {
                return Pending::ready(Err(error));
            }
            if query
                .provider
                .as_ref()
                .is_some_and(|provider| certified_provider(provider))
            {
                return session.host.query(caller, query);
            }
            // Existing general catalogue/builtin doors retain the exact
            // authenticated run. They make no SDK original-custody claim.
            let answer = self
                .broker
                .state()
                .action_query(&session.acting(caller), &query);
            return session.guarded(Arc::clone(&self.joined), caller.clone(), answer);
        }
        if !self.admitted(caller) {
            return Pending::ready(Err(ActionHostError::NotPermitted));
        }
        self.broker
            .state()
            .action_query(&on_this_device(caller), &query)
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
        if let Some(session) = &self.session {
            if let Err(error) = session.check(caller, &self.joined) {
                return Pending::ready(Err(error));
            }
            if certified_provider(&request.target.provider) {
                return session.host.invoke(caller, request);
            }
            let answer =
                self.broker
                    .state()
                    .action_invoke(&session.acting(caller), None, None, &request);
            return session.guarded(Arc::clone(&self.joined), caller.clone(), answer);
        }
        if !self.admitted(caller) {
            return Pending::ready(Err(ActionHostError::NotPermitted));
        }
        self.broker
            .state()
            .action_invoke(&on_this_device(caller), None, None, &request)
    }

    fn observe(&self, caller: &Caller, id: &tilo_actions::InvocationId) -> Option<InvocationState> {
        if let Some(session) = &self.session {
            session.check(caller, &self.joined).ok()?;
            return session.host.observe(caller, id);
        }
        if !self.admitted(caller) {
            return None;
        }
        self.broker
            .state()
            .action_observe(&on_this_device(caller), id)
    }

    /// Routed and authorized exactly as [`Self::observe`]: a caller that may
    /// not observe an invocation may not wait on it either.
    fn next_change(&self, caller: &Caller, id: &tilo_actions::InvocationId) -> Option<Pending<()>> {
        if let Some(session) = &self.session {
            session.check(caller, &self.joined).ok()?;
            return session.host.next_change(caller, id);
        }
        if !self.admitted(caller) {
            return None;
        }
        self.broker
            .state()
            .action_next_change_after_now(&on_this_device(caller), id)
    }

    fn cancel(
        &self,
        caller: &Caller,
        id: &tilo_actions::InvocationId,
    ) -> Result<(), ActionHostError> {
        if let Some(session) = &self.session {
            session.check(caller, &self.joined)?;
            return session.host.cancel(caller, id);
        }
        if !self.admitted(caller) {
            return Err(ActionHostError::UnknownInvocation);
        }
        self.broker
            .state()
            .action_cancel(&on_this_device(caller), id)
    }
}

/// The identity store's device-certificate checks, as the broker asks them.
/// This device governs the profile's own room, so a foreign runtime is not
/// held to the cap there.
struct Certificates {
    room: String,
    device: [u8; 32],
    policy_time: tilo_action_broker::peer_actions::PolicyTime,
    joined: std::sync::Weak<Joined>,
}

impl Certificates {
    fn check_registered_custody(&self, room: uuid::Uuid) -> Result<(), String> {
        let joined = self
            .joined
            .upgrade()
            .ok_or("runtime custody owner unavailable")?;
        let room = RoomId::parse(&room.to_string()).ok_or("invalid exact room")?;
        joined.recheck_mcp_executions(room)
    }
}

impl tilo_action_broker::peer_actions::CertificateAuthority for Certificates {
    fn policy_time(&self) -> Result<Option<tilo_action_broker::peer_actions::PolicyTime>, String> {
        self.policy_time
            .now_ns()
            .map_err(|error| error.to_string())?;
        Ok(Some(self.policy_time.clone()))
    }

    fn authorize_with_policy_time(
        &self,
        room: uuid::Uuid,
        cert: &tilo_user_identity::DeviceCert,
        time: &tilo_action_broker::peer_actions::PolicyTime,
    ) -> Result<(), String> {
        if !time.same_origin(&self.policy_time) {
            return Err("original runtime policy origin changed".into());
        }
        let now = time.now_ns().map_err(|error| error.to_string())?;
        self.check_registered_custody(room)?;
        tilo_user_identity::verify_device_cert_at(cert, now).map_err(|error| error.to_string())?;
        // This store check is conservative: known revocations are refused even
        // before their effective instant. It never extends a credential lease.
        tilo_identity_host::certificates::authorize(&room.to_string(), cert)
    }

    fn authorize(
        &self,
        room: uuid::Uuid,
        cert: &tilo_user_identity::DeviceCert,
    ) -> Result<(), String> {
        tilo_identity_host::certificates::authorize(&room.to_string(), cert)
    }
    fn authorize_join(
        &self,
        room: uuid::Uuid,
        cert: &tilo_user_identity::DeviceCert,
    ) -> Result<(), String> {
        let now = self
            .policy_time
            .now_ns()
            .map_err(|error| error.to_string())?;
        tilo_user_identity::verify_device_cert_at(cert, now).map_err(|error| error.to_string())?;
        self.check_registered_custody(room)?;
        tilo_identity_host::certificates::authorize_join(&room.to_string(), cert)
    }
    fn governs(&self, room: uuid::Uuid, device: Option<&[u8; 32]>) -> bool {
        uuid::Uuid::parse_str(&self.room) == Ok(room) && device.is_none_or(|d| *d == self.device)
    }
}

/// The person's camera and microphone decisions, as actions on the
/// profile's console socket (macOS, where the runtime brokers capture). A
/// socket that cannot be served leaves the runtime running without it.
fn serve_device_grants(root: &Path, apps: &apps::Apps) {
    #[cfg(target_os = "macos")]
    {
        let served = apps.device_grants().and_then(|grants| {
            crate::device_grants::serve(grants, &root.join(crate::CONSOLE_SOCKET))
        });
        if let Err(error) = served {
            tracing::warn!(%error, "the device grants are not served on the console socket");
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (root, apps);
}

/// The broker's channel state, with the profile's durable collaboration
/// history. Without it scrollback is limited to memory, as in the desktop
/// session, and the host still serves.
fn channels(root: &Path) -> BrokerChannels {
    let mut channels = BrokerChannels::default();
    match CollabHistoryStore::open(root.join(COLLABORATION_HISTORY)) {
        Ok(history) => channels.history = Some(history),
        Err(error) => tracing::warn!(
            %error,
            "durable collaboration history unavailable; scrollback is limited to memory"
        ),
    }
    channels
}

/// This device's collaboration identity, provisioned in `storage` on first
/// start; answers its device key.
fn provision_device(storage: IdentityStorage) -> io::Result<[u8; 32]> {
    let identity_dir = runtime_identity::identity_dir_from_env()?;
    let paths = tilo_identity_host::provisioning::collaboration_identity_paths();
    tilo_identity_host::provisioning::ensure_foreign_collaboration_identity(
        &identity_dir,
        &paths,
        storage,
    )?;
    Ok(tilo_user_identity::persist::load_device_key(&paths)
        .map_err(io::Error::other)?
        .verifying_key()
        .to_bytes())
}

/// The profile's workspace files, shown at their folder in this user's
/// home. Without the folder the host still serves: apps keep their
/// documents, only the folder view of the files is missing. A profile the
/// desktop session serves has none: that host projects its files itself.
fn workspace_files(
    profile: &Path,
    workspace: &ProfileWorkspace,
    media: &profile::MediaHost,
    endpoint: iroh::Endpoint,
    author: &files::Author,
    stage: &mut dyn FnMut(&str),
) -> Option<files::WorkspaceFiles> {
    if !workspace.governs() {
        return None;
    }
    stage("showing the workspace files");
    let started = files::folder(&workspace.name()).and_then(|folder| {
        let projection = files::Projection::for_host();
        files::WorkspaceFiles::start(
            profile,
            &folder,
            media.store(),
            endpoint,
            author,
            projection,
        )
    });
    started
        .inspect_err(|error| tracing::warn!(%error, "the workspace folder is unavailable"))
        .ok()
}

/// The durable owner of the profile's room: this host's own, governed with
/// the device key, or for a profile adopted from the desktop session one
/// that refuses every commit.
fn room_owner(
    profile: &Path,
    room: &str,
    workspace: &Arc<ProfileWorkspace>,
    apps: &Arc<apps::Apps>,
    dev: bool,
    keys: tilo_action_broker::channel_encryption::ChannelKeyProvider,
) -> io::Result<Arc<dyn OwnerCommitService>> {
    if !workspace.governs() {
        return Ok(Arc::new(AdoptedRoom {
            room: room.to_owned(),
        }));
    }
    let apps = Arc::clone(apps);
    let creation = RecordCreation {
        declared_kinds: Box::new(move |app| apps.declared_resource_kinds(app)),
        dev,
    };
    let device = tilo_identity_host::broker::trusted_current_device(room)?;
    let owner = RoomOwner::open(profile, room, device, creation, Arc::clone(workspace), keys)?;
    Ok(Arc::new(owner))
}

/// The joined rooms and this device's channel keys for them and for the
/// profile's own room, under the device's trusted custody.
fn joined_rooms_and_keys(
    profile: &Path,
    room: &str,
    storage: IdentityStorage,
    broker: (Arc<Mutex<BrokerChannels>>, Arc<FanoutState>),
) -> io::Result<(Arc<Joined>, channels::Channels)> {
    let custody = tilo_identity_host::broker::trusted_current_device(room)?;
    let channels = Arc::clone(&broker.0);
    let joined = joined_rooms(profile, room, custody.clone(), broker)?;
    let keys = channels::open(
        profile,
        RoomId::parse(room).ok_or_else(|| io::Error::other("invalid runtime room"))?,
        storage,
        custody.0,
        Arc::clone(&joined),
        channels,
    )?;
    Ok((joined, keys))
}

/// The rooms this profile joined, synced as this device over an endpoint
/// keyed by its device key. It refuses to enroll in its own room and in the
/// rooms the desktop session already enrolled this device in.
fn joined_rooms(
    profile: &Path,
    room: &str,
    (key, cert): (ed25519_dalek::SigningKey, tilo_user_identity::DeviceCert),
    broker: (Arc<Mutex<BrokerChannels>>, Arc<FanoutState>),
) -> io::Result<Arc<Joined>> {
    let custody = tilo_identity_host::custody::HostIdentity {
        device_cert: cert,
        device_signing_key: key.clone(),
        workspace_device_certs: std::collections::HashMap::new(),
        workspace_revocations: std::collections::HashMap::new(),
        identity_paths: Some(tilo_identity_host::provisioning::collaboration_identity_paths()),
    };
    let endpoint = tilo_rt::app()
        .handle()
        .block_on(
            iroh::Endpoint::builder(iroh::endpoint::presets::N0)
                .secret_key(iroh::SecretKey::from_bytes(&key.to_bytes()))
                .bind(),
        )
        .map_err(io::Error::other)?;
    let mut refused = joined::desktop_enrolled(profile)?;
    refused.extend(RoomId::parse(room));
    Joined::open(profile, custody, endpoint, refused, broker)
}

#[cfg(test)]
#[path = "standalone_tests.rs"]
mod tests;
