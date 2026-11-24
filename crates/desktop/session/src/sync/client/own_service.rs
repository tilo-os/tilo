//! This device's own workspace service in the session: a member copy of each workspace the
//! device takes part in, kept beside the device's daemon, which founds, joins and keeps every
//! workspace. The session's broker keeps what it has no daemon service for yet on it: the
//! workspaces' saved shortcuts, their events and the assistant's look-round over them.
//!
//! The service takes part in a workspace's key groups as this device: it asks this device's key
//! service for their keys, and hands what its members send for the groups to it, so the
//! service and the device never found two groups under one leaf.

use std::sync::Arc;

use crate::engine::Engine;
use neighbour_wire::NeighbourMsg;
use tilo_endpoint_reachability::Reachability;
use tilo_identity::identity_store::DeviceIdentity;
use tilo_network_actor::own_service::{OwnService, workspace_service};
use tilo_workspace_service::workspaces::{DeviceKeyGroups, KeySink};

/// Open this device's workspace service under `engine`'s state, as `identity`, on `endpoint`,
/// waking the relays `reachability` holds back for the invitations it signs, and serve it
/// through `own` from now on.
///
/// # Errors
/// The engine keeps no state, this device cannot certify the service's own key, or the
/// service's store cannot be opened.
pub(super) async fn open(
    own: &OwnService,
    identity: &DeviceIdentity,
    engine: &Arc<Engine>,
    (endpoint, reachability): (&iroh::Endpoint, &Reachability),
) -> Result<Arc<workspace_service::WorkspaceService>, String> {
    let handed = Arc::downgrade(engine);
    // A record this device holds sealed under an epoch its group has moved past opens only when
    // the group's frames arrive: the person's other devices hand them to it, as the workspace's
    // host would when it is there.
    let behind = Arc::downgrade(engine);
    let groups = tilo_workspace_service::own_device::key_groups(
        engine.keys(),
        Some(Box::new(move |workspace| {
            tilo_desktop_model::WorkspaceId::parse(workspace)
                .and_then(|id| handed.upgrade()?.handed_keys(id))
        })),
        Some(Arc::new(DeviceKeySink {
            engine: Arc::downgrade(engine),
        })),
        Some(Arc::new(move |group, held| {
            if let Some(link) = behind.upgrade().and_then(|engine| engine.link()) {
                link.want_frames(group, held);
            }
        })),
    );
    open_with_groups(own, identity, engine, (endpoint, reachability), groups).await
}

/// [`open`] with `groups` keeping the workspaces' keys: keys a test makes itself, where the
/// session opens no key service of its own.
pub(crate) async fn open_with_groups(
    own: &OwnService,
    identity: &DeviceIdentity,
    engine: &Arc<Engine>,
    (endpoint, reachability): (&iroh::Endpoint, &Reachability),
    groups: DeviceKeyGroups,
) -> Result<Arc<workspace_service::WorkspaceService>, String> {
    let root = engine
        .service_root()
        .ok_or("this session keeps no state, so it opens no workspace service")?;
    let service = tilo_workspace_service::own_device::open(
        root,
        tilo_workspace_service::own_device::Device {
            cert: &identity.device_cert,
            signer: &identity.signer,
            itself: None,
        },
        endpoint.clone(),
        groups,
        |config| workspace_service::ServiceConfig {
            // The desktop's first frame waits on this service, and its endpoint holds its relays
            // back until something needs the device reachable: the service names its address
            // again once a relay comes, and a desktop never waits on the network to show.
            reaching: tilo_endpoint_reachability::Reaching::WhenNeeded,
            // The service makes the links this device hands out, and an invitation's address is
            // its to sign: it wakes these relays and waits up to a net report for one before it
            // does.
            reachability: reachability.clone(),
            ..config
        },
    )
    .await
    .map_err(|error| error.to_string())?;
    let service = own.install(Arc::new(service));
    Ok(service)
}

/// Where the service hands what its members send for a workspace's key groups: this device's
/// key service, through the link the session's key groups hear on.
struct DeviceKeySink {
    engine: std::sync::Weak<Engine>,
}

impl KeySink for DeviceKeySink {
    fn take(&self, messages: Vec<NeighbourMsg>) {
        let Some(link) = self.engine.upgrade().and_then(|engine| engine.link()) else {
            return;
        };
        for message in &messages {
            link.heard(message);
        }
    }

    /// Nothing to note: this device's key service reads the workspace's governance itself,
    /// through its own session to the service.
    fn settled(&self, _workspace: &str, _state: tilo_peer_state::StatePeer) {}
}

/// Open this device's own workspace service when no peer network opened it: the session takes
/// part in none, or its endpoint did not bind. The service is the one a networked session opens,
/// on the same store, on an endpoint under the device's key that only this process reaches; so
/// what the session's broker keeps on it (the workspaces' saved shortcuts, their events) is kept
/// with no network too, and once a session's network comes up it carries the same store.
pub(crate) fn open_alone(
    engine: &Arc<Engine>,
    service: tilo_workspace_service::service::ServiceSlot,
) {
    if service.service().is_some() {
        return;
    }
    let identity = engine.identity().clone();
    let (own, _unrouted) = OwnService::deferred_in(service);
    let opening = async {
        let secret = identity
            .signer
            .endpoint_secret()
            .ok_or("this device's key never leaves its store, so it opens no workspace service")?;
        let endpoint = super::loopback_endpoint()?
            .secret_key(iroh::SecretKey::from_bytes(&secret))
            .bind()
            .await
            .map_err(|error| format!("this device's own endpoint: {error}"))?;
        // Only this process reaches this endpoint, so nothing is held back for a link it makes;
        // the service keeps it for as long as it runs.
        open(
            &own,
            &identity,
            engine,
            (&endpoint, &Reachability::default()),
        )
        .await
        .map(drop)
    };
    let opened = tilo_rt::try_app()
        .map_err(|error| error.to_string())
        .and_then(|runtime| runtime.block_on(opening).map_err(|error| error.to_string()))
        .and_then(|opened| opened);
    if let Err(error) = opened {
        tracing::warn!(%error, "this device's workspace service did not open");
    }
}
