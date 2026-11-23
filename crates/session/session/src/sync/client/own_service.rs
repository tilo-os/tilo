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
        true,
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

/// Found the person's own workspace when this device made their root and has not founded it yet
/// (the mark [`tilo_identity::provisioning::founds_person_workspace`] reads). The mark stays until
/// this device holds the workspace, so a founding that fails is tried again on the next start.
#[cfg(target_os = "linux")]
pub(super) async fn found_person_workspace(engine: &Engine) {
    let Ok(dir) = tilo_identity::store::identity_dir_from_env() else {
        return;
    };
    if !tilo_identity::provisioning::founds_person_workspace(&dir) {
        return;
    }
    let id = tilo_server_protocol::servers::person_workspace_id(
        &engine.identity().device_cert.user_public_key,
    );
    let Some(workspace) = tilo_desktop_model::WorkspaceId::parse(&id) else {
        tracing::error!(%id, "a person's workspace id does not read as a workspace id");
        return;
    };
    let name = tilo_workspace_service::workspaces::person_workspace::NAME;
    if let Err(error) = engine.found(workspace, name).await {
        tracing::warn!(%error, "this device did not found its person's own workspace; it tries again on its next start");
        return;
    }
    if !engine.holds(workspace) {
        return;
    }
    if let Err(error) = tilo_identity::provisioning::clear_founding_mark(&dir) {
        tracing::warn!(%error, "the mark that this device founds its person's workspace was not taken away");
    }
}

/// Note the links out of the workspaces this device holds, as its own service lists them: they
/// were made before this session, and whoever redeems one dials this device
/// ([`Engine::invitations_open`]).
pub(super) async fn note_open_links(engine: &Arc<Engine>) {
    for workspace in engine.workspaces() {
        let named = workspace.to_string();
        if !engine.is_holder(&named) {
            continue;
        }
        let Some(asked) = engine.ask_holder(
            workspace,
            tilo_workspace_service::workspaces::INVITATIONS,
            serde_json::Map::new(),
        ) else {
            return;
        };
        match asked.await {
            Ok(listed) => engine.note_invitations(workspace, &listed),
            Err(error) => {
                tracing::warn!(%error, %named, "the links out of a workspace were not listed");
            }
        }
    }
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
    serve_alone(engine, (service, records), None, |founder| founder);
}

/// [`found_alone`] with the workspaces' keys kept by `groups`, where the session opens no key
/// service of its own: a test's keys, so its made workspaces seal as a device's do. `wrap` is
/// handed the founder before it serves, for a caller that founds elsewhere.
pub(crate) fn serve_alone(
    engine: &Arc<Engine>,
    (service, records): (
        tilo_workspace_service::service::ServiceSlot,
        crate::platform::records::Changes,
    ),
    groups: Option<DeviceKeyGroups>,
    wrap: impl FnOnce(Arc<dyn crate::engine::Founder>) -> Arc<dyn crate::engine::Founder>,
) {
    if engine.founds() {
        return;
    }
    let identity = engine.identity().clone();
    let (own, _unrouted) = OwnService::deferred_in(service);
    let mut groups = groups;
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
        match groups.take() {
            Some(groups) => {
                open_with_groups(
                    &own,
                    &identity,
                    engine,
                    (&endpoint, &Reachability::default()),
                    groups,
                    records,
                )
                .await?;
            }
            None => {
                open(
                    &own,
                    &identity,
                    engine,
                    (&endpoint, &Reachability::default()),
                    records,
                )
                .await?;
            }
        }
        Ok::<_, String>(endpoint)
    };
    let opened = tilo_rt::try_app()
        .map_err(|error| error.to_string())
        .and_then(|runtime| runtime.block_on(opening).map_err(|error| error.to_string()))
        .and_then(|opened| opened);
    match opened {
        Ok(endpoint) => engine.founders().serve(wrap(Arc::new(Founding {
            service: own,
            endpoint,
            link: engine.link(),
            identity,
            engine: Arc::downgrade(engine),
            network: None,
            actions: None,
        }))),
        Err(error) => {
            tracing::warn!(%error, "this device's workspace service did not open, so it founds no workspace");
        }
    }
}

/// Founds a workspace this device made on its own workspace service: the service creates it and
/// invites this device, and this device redeems the invitation in process, as it redeems any.
pub(super) struct Founding {
    pub(super) service: OwnService,
    pub(super) endpoint: iroh::Endpoint,
    pub(super) identity: DeviceIdentity,
    pub(super) engine: std::sync::Weak<Engine>,
    /// The peer network the workspace is carried on: `None` while this device takes part in none,
    /// when it is held here and carried once a session's network joins it.
    pub(super) network: Option<(
        tokio::sync::mpsc::Sender<tilo_network_actor::NetCmd>,
        tokio::sync::mpsc::Sender<tilo_network_actor::NetEvent>,
    )>,
    pub(super) link: Option<Arc<crate::workspace_keys::KeyLink>>,
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
        Box::pin(async move {
            let service = self
                .service
                .get()
                .ok_or("this device's workspace service is not open")?;
            let engine = self.engine.upgrade().ok_or("the session has stopped")?;
            let invitation = service
                .place_own_workspace(&workspace.to_string(), &name, &self.identity.device_cert)
                .await?;
            drop(service);
            super::adopt::redeem(
                self.endpoint.clone(),
                self.identity.clone(),
                Arc::clone(&engine),
                self.network
                    .as_ref()
                    .map(|(network, events)| super::adopt::Carrier { network, events }),
                crate::sync::Enrolling {
                    workspace_id: workspace,
                    way: tilo_workspace_directory::Way::Invitation(invitation),
                    name,
                },
                self.link.as_deref(),
                Some(&self.service),
            )
            .await
            .map_err(|failure| failure.to_string())?;
            // This device created the workspace, so its key service creates the group the
            // workspace's records are sealed with: the founding hands it the group before
            // anything is kept there. Nothing else asks for a group nothing asked of yet, and
            // a first write refused for its keys would wait on a change no pass makes.
            hand_the_group(&engine, workspace).await;
            Ok(())
        })
    }

    fn ask(
        &self,
        workspace: tilo_desktop_model::WorkspaceId,
        action: &str,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) -> Option<tilo_session_client::engine::ServiceAnswer> {
        let engine = self.engine.upgrade()?;
        let named = workspace.to_string();
        let request = governance_request(&named, action, arguments);
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

/// Hand this device's key service the group of `workspace`, a workspace this device founded: it
/// creates the group and holds its keys, and what is kept in the workspace next finds them.
///
/// A group starts when an endpoint asks for its keys, which is what
/// [`KeyProvider::group_keys`](tilo_workspace_keys::KeyProvider::group_keys) does with `create`:
/// the founder creates it on the key service's next pass, once the workspace's governance names
/// this device, which is after its enrollment. Nothing else asks before the first record is
/// kept, so a founding that left the group to a later ask could leave a first write waiting for
/// keys that no pass brings. A workspace another device founded is not created here: the group
/// arrives as that device admits this one.
async fn hand_the_group(engine: &Engine, workspace: tilo_desktop_model::WorkspaceId) {
    let founded = engine
        .owner()
        .with_current(workspace, |view| Ok(view.founded_here()))
        .unwrap_or(false);
    if !founded {
        return;
    }
    if let Some(Err(error)) = engine
        .keys()
        .group_keys(
            &workspace.to_string(),
            collab_types::EVERY_MEMBER,
            true,
            tilo_workspace_service::own_device::OPEN_WAIT,
        )
        .await
    {
        tracing::debug!(%workspace, %error, "a founded workspace's group keys are not here yet");
    }
}

/// What a request no holder answered says.
const UNANSWERED: &str = "the workspace's holder dropped the request";

/// `workspace`'s governance action `action` with `arguments`, as an invocation of it.
fn governance_request(
    workspace: &str,
    action: &str,
    arguments: serde_json::Map<String, serde_json::Value>,
) -> InvokeRequest {
    InvokeRequest {
        invocation: tilo_actions::InvocationId::new(),
        target: RemoteActionTarget::workspace_governance(workspace, action),
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
