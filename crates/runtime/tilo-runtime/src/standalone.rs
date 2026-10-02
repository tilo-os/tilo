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
                })),
                launchable_catalogue: Some(root.join("state/launchable-actions.json")),
                launcher: Some(Arc::clone(&apps) as Arc<dyn LauncherServices>),
                app_action_access: Some(apps.access_resolver()),
                app_action_grants: Some(Arc::new(ProfileGrants {
                    workspace: Arc::clone(&workspace),
                    apps,
                })),
                legacy_workspace: Some(Arc::clone(&workspace) as Arc<dyn LegacyWorkspaceProjection>),
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

    fn actor(&self) -> &str {
        &self.actor
    }

    fn action_host(&self) -> io::Result<RuntimeActionHost> {
        self.broker
            .app_action_broker()
            .map(RuntimeActionHost)
            .ok_or_else(|| io::Error::other("runtime action broker is unavailable"))
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
pub struct RuntimeActionHost(AppActionBroker);

fn on_this_device(caller: &Caller) -> Caller {
    caller.clone().from_foreign_device(true)
}

impl ActionHost for RuntimeActionHost {
    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionHostError>> {
        self.0.state().action_query(&on_this_device(caller), &query)
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
        self.0
            .state()
            .action_invoke(&on_this_device(caller), None, None, &request)
    }

    fn observe(&self, caller: &Caller, id: &tilo_actions::InvocationId) -> Option<InvocationState> {
        self.0.state().action_observe(caller, id)
    }

    fn cancel(
        &self,
        caller: &Caller,
        id: &tilo_actions::InvocationId,
    ) -> Result<(), ActionHostError> {
        self.0.state().action_cancel(caller, id)
    }
}

/// The identity store's device-certificate checks, as the broker asks them.
/// This device governs the profile's own room, so a foreign runtime is not
/// held to the cap there.
struct Certificates {
    room: String,
    device: [u8; 32],
}

impl tilo_action_broker::peer_actions::CertificateAuthority for Certificates {
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
