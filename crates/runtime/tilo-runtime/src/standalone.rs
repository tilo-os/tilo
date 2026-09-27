//! Services for a runtime profile without the Tilo desktop: the shared
//! action broker and identity service over the profile's own workspace.
//!
//! This is what the runtime serves on stock macOS or a foreign Linux
//! desktop. The desktop session hosts the same brokers with its native
//! adapters (launcher, governance, durable owner, peer network). Here the
//! profile launches its installed apps through the platform launcher
//! ([`crate::apps`]) and records the grants people give apps for other
//! apps' actions in its own workspace; every other host service is absent,
//! and the broker answers as it does for any host that does not provide one.
//! What this host keeps durably is in the profile: grants, effect and task
//! records in its own workspace file, the invocation journal and
//! collaboration history.

mod workspace;

use crate::{BLOB_SOCKET, COLLAB_SOCKET, IDENTITY_SOCKET, RuntimeServices, apps, profile, unix};
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
use tilo_identity_host::{
    broker::{IdentityBrokerHandle, WorkspaceMembership},
    provisioning::IdentityStorage,
};
use workspace::{ProfileGrants, ProfileWorkspace};

/// Durable collaboration history, where the desktop session keeps it under
/// the profile's `XDG_STATE_HOME`.
const COLLABORATION_HISTORY: &str = "state/tilo/collaboration/history";

/// A standalone service session over one profile. Drop applications first.
pub struct Standalone {
    broker: BrokerHandle,
    _identity: IdentityBrokerHandle,
    room: String,
    actor: String,
    identity_socket: PathBuf,
    blob_socket: PathBuf,
    _media: profile::MediaHost,
    _lock: advisory_lock::FileLock,
}

impl RuntimeServices for Standalone {
    type ActionHost = RuntimeActionHost;

    fn start_reporting(root: &Path, stage: &mut dyn FnMut(&str)) -> io::Result<Self> {
        stage("locking the profile");
        let lock = profile::lock(root)?;
        stage("provisioning the collaboration identity");
        let identity_dir = runtime_identity::identity_dir_from_env()?;
        tilo_identity_host::provisioning::ensure_foreign_collaboration_identity(
            &identity_dir,
            &tilo_identity_host::provisioning::collaboration_identity_paths(),
            IdentityStorage::Platform,
        )?;
        stage("loading the workspace");
        let workspace = ProfileWorkspace::open(root)?;
        let room_id = workspace.room();
        let actor = tilo_identity_host::registry::selected_identity_profile(&room_id)?
            .ok_or_else(|| io::Error::other("runtime workspace has no selected identity"))?
            .principal_id();
        stage("saving the workspace");
        workspace.adopt(&actor)?;
        let workspace = Arc::new(workspace);
        let apps = Arc::new(apps::Apps::from_env(root, &room_id));
        let identity_socket = root.join(IDENTITY_SOCKET);
        stage("starting the identity service");
        let identity = tilo_identity_host::broker::spawn(
            identity_socket.clone(),
            Arc::clone(&workspace) as Arc<dyn WorkspaceMembership>,
            Arc::new(()),
        )?;
        stage("starting the collaboration broker");
        let broker = tilo_action_broker::collab_broker::spawn_with_services(
            root.join(COLLAB_SOCKET),
            Some(Arc::clone(&workspace) as Arc<dyn WorkspaceAuthority>),
            PeerPublisher::disabled(),
            None,
            HostServices {
                native_certificates: Some(Arc::new(Certificates)),
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
            Arc::new(Mutex::new(channels(root))),
            Arc::new(FanoutState::new()),
        )?;
        let blob_socket = root.join(BLOB_SOCKET);
        stage("opening the content store");
        let media = profile::MediaHost::start(&root.join("content"), &blob_socket)?;
        Ok(Self {
            broker,
            _identity: identity,
            room: room_id,
            actor,
            identity_socket,
            blob_socket,
            _media: media,
            _lock: lock,
        })
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
}

/// The broker's action door as one more local caller, standing for no app
/// connection.
#[derive(Clone)]
pub struct RuntimeActionHost(AppActionBroker);

impl ActionHost for RuntimeActionHost {
    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionHostError>> {
        self.0.state().action_query(caller, &query)
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
        self.0.state().action_invoke(caller, None, None, &request)
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
struct Certificates;

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

#[cfg(test)]
#[path = "standalone_tests.rs"]
mod tests;
