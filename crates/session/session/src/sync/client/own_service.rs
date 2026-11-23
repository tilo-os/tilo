//! This device's own workspace service: the same service a server has, in this process,
//! holding the workspaces this device founds.
//!
//! The desktop governs no workspace itself. A workspace made here is placed on this service,
//! and the device's own engine enrolls in it as a member, as it would in a workspace held
//! anywhere else: its session goes to the service over an in-process carrier, and the person's
//! other devices and the people they invite reach the service over iroh, on the ALPNs it serves
//! on this endpoint.
//!
//! The service takes part in a workspace's key groups as this device: it asks this device's key
//! service for their keys, and hands what its members send for the groups to it, so the
//! service and the device never found two groups under one leaf.

use std::sync::Arc;

use crate::engine::Engine;
use neighbour_wire::NeighbourMsg;
use tilo_actions::InvocationUpdate;
use tilo_actions::remote::{
    ActionProvider as _, Caller, InvokeArgument, InvokeRequest, ReceiptStatus, RemoteActionTarget,
};
use tilo_endpoint_reachability::Reachability;
use tilo_identity::identity_store::DeviceIdentity;
use tilo_network_actor::own_service::{OwnService, workspace_service};
use tilo_peer_state::actions::{ActionService, FOLLOW_DEADLINE};
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
    records: crate::platform::records::Changes,
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
    open_with_groups(
        own,
        identity,
        engine,
        (endpoint, reachability),
        groups,
        records,
    )
    .await
}

/// [`open`] with `groups` keeping the workspaces' keys: keys a test makes itself, where the
/// session opens no key service of its own.
pub(crate) async fn open_with_groups(
    own: &OwnService,
    identity: &DeviceIdentity,
    engine: &Arc<Engine>,
    (endpoint, reachability): (&iroh::Endpoint, &Reachability),
    groups: DeviceKeyGroups,
    records: crate::platform::records::Changes,
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
    // The cloud service's file accounts, and their sign-ins, are the workspaces' too.
    #[cfg(target_os = "linux")]
    {
        let workspaces = service.workspaces().clone();
        let by = Caller::authenticated(
            identity.device_cert.user_public_key,
            identity.device_cert.device_public_key,
        );
        crate::account_sign_ins::serve(workspaces, by, records);
    }
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

/// Found the workspaces `engine` makes on this device's own workspace service when no peer network
/// opened it: the session takes part in none, or its endpoint did not bind. The service is the one
/// a networked session opens, on the same store, on an endpoint under the device's key that only
/// this process reaches; so a desktop with no network keeps its layout through a restart, and
/// once a session's network comes up it carries what was founded here, with nothing to move.
pub(crate) fn found_alone(
    engine: &Arc<Engine>,
    service: tilo_workspace_service::service::ServiceSlot,
    records: crate::platform::records::Changes,
) {
    if engine.founds() {
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
        // Only this process reaches this endpoint, so nothing is held back for a link it makes.
        open(
            &own,
            &identity,
            engine,
            (&endpoint, &Reachability::default()),
            records,
        )
        .await?;
        Ok::<_, String>(endpoint)
    };
    let opened = tilo_rt::try_app()
        .map_err(|error| error.to_string())
        .and_then(|runtime| runtime.block_on(opening).map_err(|error| error.to_string()))
        .and_then(|opened| opened);
    match opened {
        Ok(endpoint) => engine.founders().serve(Arc::new(Founding {
            service: own,
            endpoint,
            identity,
            engine: Arc::downgrade(engine),
            actions: None,
        })),
        Err(error) => {
            tracing::warn!(%error, "this device's workspace service did not open, so it founds no workspace");
        }
    }
}

/// This device's own workspace service as the engine's founder: it founds nothing, since the
/// device's daemon founds every workspace, and asks a workspace held elsewhere of its host.
pub(super) struct Founding {
    pub(super) service: OwnService,
    pub(super) endpoint: iroh::Endpoint,
    pub(super) identity: DeviceIdentity,
    pub(super) engine: std::sync::Weak<Engine>,
    /// The session's device action service, which asks a workspace held elsewhere of its holder:
    /// `None` while this device takes part in no peer network.
    pub(super) actions: Option<ActionService>,
}

impl tilo_session_client::engine::Founder for Founding {
    fn found(
        &self,
        workspace: tilo_desktop_model::WorkspaceId,
        name: String,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send + '_>> {
        // A workspace is founded by the device's daemon, which keeps it
        // (`account/workspaces#create`): the session's own service founds none.
        let _ = (workspace, name);
        Box::pin(std::future::ready(Err(
            "a workspace is founded by this device's daemon".to_owned(),
        )))
    }

    fn ask(
        &self,
        workspace: tilo_desktop_model::WorkspaceId,
        target: RemoteActionTarget,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) -> Option<tilo_session_client::engine::ServiceAnswer> {
        let engine = self.engine.upgrade()?;
        let named = workspace.to_string();
        let request = holder_request(target, arguments);
        // The service holds a workspace this device is the holder of: it shares this device's key,
        // and answers it here as it answers every other member's device.
        if engine.is_holder(&named) {
            let cert = &self.identity.device_cert;
            let caller = Caller::authenticated(cert.user_public_key, cert.device_public_key)
                .in_workspace(named);
            let invoked = self
                .service
                .get()?
                .workspaces()
                .device_actions()
                .invoke(&caller, request);
            return Some(Box::pin(async move {
                match invoked.await {
                    Some(Ok(receipt)) => settled(receipt.status),
                    Some(Err(error)) => Err(error.to_string()),
                    None => Err(UNANSWERED.to_owned()),
                }
            }));
        }
        // Held elsewhere: its holder is asked at the address its enrollment names, as the device
        // that holder is (a desktop's workspace service is its desktop's).
        let (_, addr) = engine.holder_address(workspace)?;
        let holder = engine.holder_of(workspace)?;
        let actions = self.actions.as_ref()?;
        actions.reach_device_at(holder, *addr.id.as_bytes());
        let followed = std::sync::Arc::new(actions.remote(&self.endpoint, &addr, &named))
            .invoke_until_settled(request, FOLLOW_DEADLINE);
        Some(Box::pin(async move {
            match followed.await {
                Some(Ok(InvocationUpdate::Succeeded { output, .. })) => {
                    Ok(output.value.unwrap_or_default())
                }
                Some(Ok(InvocationUpdate::Failed { error, .. })) => Err(error.message),
                Some(Ok(_)) => Err("the workspace's holder cancelled the request".to_owned()),
                Some(Err(error)) => Err(error.to_string()),
                None => Err(UNANSWERED.to_owned()),
            }
        }))
    }

    /// The host's own operations (`tilo.host`): what the person plans and
    /// reads here, whether or not this device holds the workspace yet. The
    /// service makes the workspace a plan needs when the first machine is
    /// planned into it.
    fn ask_host(
        &self,
        action: &str,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) -> Option<tilo_session_client::engine::ServiceAnswer> {
        let service = self.service.get()?;
        let caller = self.identity.device_cert.clone();
        let action = action.to_owned();
        Some(Box::pin(async move {
            service.ask_own(&caller, &action, arguments).await
        }))
    }
}

/// What a request no holder answered says.
const UNANSWERED: &str = "the workspace's holder dropped the request";

/// The action `target` with `arguments`, as an invocation of it.
fn holder_request(
    target: RemoteActionTarget,
    arguments: serde_json::Map<String, serde_json::Value>,
) -> InvokeRequest {
    InvokeRequest {
        invocation: tilo_actions::InvocationId::new(),
        target,
        argument: InvokeArgument::Json(arguments),

        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    }
}

/// What a receipt the governance answered at once comes to.
fn settled(status: ReceiptStatus) -> Result<serde_json::Value, String> {
    match status {
        ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or_default()),
        ReceiptStatus::Rejected { error } => Err(error.to_string()),
        ReceiptStatus::Failed { error } => Err(error.message),
        ReceiptStatus::Accepted | ReceiptStatus::Cancelled { .. } => {
            Err("the request did not finish".to_owned())
        }
    }
}
