//! The relay's live origin: one `moq-lite` relay every carrier of a workspace's
//! live tracks reaches, and the authority that admits them to it.
//!
//! The live origin belongs to the relay, not to a listener: every device dials
//! it over the relay's iroh endpoint ([`tilo_live::native`]) as its own
//! certificate says it may. A page dials it the same way: its device
//! certificate is the grant, and the delegation it carries names the endpoint
//! key of its page, which cannot leave ownership. Either way the workspace's
//! governance answers for it as it answers for any member, and the answer
//! stays current from the relay-wide authority epoch — a device removed from
//! the workspace is cut on that event, not on a timer.
//!
//! A call's media names its call, and the live origin carries it only as the
//! workspace's rule for that call lets each device's person: an ended call's
//! tracks go nowhere, and a person denied a call neither sends nor is sent any
//! of it, whatever their presence says. The rules are read from the workspace
//! as it stands, again on every change of authority.
//!
//! The sessions themselves are `tilo-live`'s: this module answers who may open
//! one, and [`Relay::serve_session`] runs the same admission preface,
//! `moq-lite` session and copying relay for every carrier.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use collab_types::calls::CallRule;
use tilo_live::live::Namespace;
use tilo_live::relay::{
    Admission, Denied, Direction, Peer, Relay, RelayAuthority, SessionAuthorization,
};
use tilo_live::scope::Scope;
use tilo_lock::MutexRecover as _;
use tokio::sync::watch;

use collab_types::WorkspaceId;
use tilo_server_protocol::auth::AuthenticatedPeer;
use tilo_workspace_service::service::ServerProtocol;
use tilo_workspace_service::workspace_access::{self, WorkspaceOperation};

/// One device's live session: whose it is, the scope it publishes in, and
/// whether the workspace still admits it.
struct Grant {
    workspace: WorkspaceId,
    peer: AuthenticatedPeer,
    /// The namespace the device publishes its own tracks under: one
    /// device of the workspace scope, as every device's is.
    namespace: String,
    /// The subtree every device of the same scope publishes in.
    scope: String,
    /// Whether this device is a hosted browser environment's device: it
    /// publishes its window's page and is offered nobody else's tracks, so no
    /// call, camera, screen or other window of the workspace reaches it.
    environment: bool,
    admitted: AtomicBool,
    /// What each of the workspace's calls allows, by the tag its tracks name
    /// ([`collab_types::calls::tag`]).
    calls: Mutex<Arc<Calls>>,
    /// Reports each change of `admitted` or `calls`: the relay re-observes on
    /// it and cuts a session, or a call track, whose answer narrowed.
    changed: watch::Sender<u64>,
}

/// What each call of a workspace allows, by the tag its tracks name.
type Calls = BTreeMap<String, CallRule>;

impl Grant {
    fn new(
        workspace: WorkspaceId,
        workspace_name: &str,
        peer: AuthenticatedPeer,
        environment: bool,
    ) -> Self {
        let namespace = Namespace::new(&Scope::workspace(workspace_name), peer.device_public_key);
        Self {
            workspace,
            peer,
            scope: namespace.scope_path(),
            namespace: namespace.path(),
            environment,
            admitted: AtomicBool::new(true),
            calls: Mutex::new(Arc::default()),
            changed: watch::channel(0_u64).0,
        }
    }

    /// Hold the workspace's calls to `calls` from now on, waking the relay
    /// when they moved.
    fn set_calls(&self, calls: Arc<Calls>) {
        let moved = {
            let mut held = self.calls.lock_recover();
            let moved = **held != *calls;
            *held = calls;
            moved
        };
        if moved {
            self.changed
                .send_modify(|revision| *revision = revision.wrapping_add(1));
        }
    }

    /// Whether the workspace admits this device now.
    fn admitted(&self) -> bool {
        self.admitted.load(Ordering::Acquire)
    }

    /// Record the current answer, waking the relay when it changes.
    fn set(&self, admitted: bool) {
        if self.admitted.swap(admitted, Ordering::AcqRel) != admitted {
            self.changed
                .send_modify(|revision| *revision = revision.wrapping_add(1));
        }
    }
}

impl SessionAuthorization for Grant {
    fn publication(&self) -> Option<&str> {
        Some(&self.namespace)
    }

    fn subscriptions(&self) -> &[String] {
        &[]
    }

    fn subscription_prefix(&self) -> Option<&str> {
        // A browser environment's device hears nobody: its session publishes
        // its window's page, and a viewer's ask arrives as a demand on that
        // track, not as a subscription of its own. So no track of the
        // workspace — a call, a camera, a screen, another window's page — is
        // offered to it, whatever its session asks for.
        (!self.environment).then_some(self.scope.as_str())
    }

    fn current(&self, direction: Direction, namespace: &str) -> bool {
        self.admitted()
            && match direction {
                Direction::Publish => namespace == self.namespace,
                // A browser environment's device is offered no track of the
                // workspace, whoever asks: no call, camera, screen or other
                // window reaches it.
                Direction::Subscribe => {
                    !self.environment
                        && namespace
                            .strip_prefix(&self.scope)
                            .is_some_and(|rest| rest.starts_with("/device/"))
                }
            }
    }

    fn changed(&self) -> Option<watch::Receiver<u64>> {
        Some(self.changed.subscribe())
    }

    fn call(&self, direction: Direction, call: &str) -> bool {
        let person = hex::encode(self.peer.user_public_key);
        let may = self
            .calls
            .lock_recover()
            .get(call)
            .map_or(collab_types::calls::Directions::BOTH, |rule| {
                rule.may(Some(&person))
            });
        self.admitted()
            && match direction {
                Direction::Publish => may.publish,
                Direction::Subscribe => may.subscribe,
            }
    }
}

/// One live session of an environment's device, as the relay counts it: while
/// the session lasts, the device is here, and its environment is not stopped
/// for want of a session ([`tilo_workspace_service::environment::EnvironmentRegistry`]). Dropped
/// when the session ends, whichever way it ended.
struct Present {
    grant: Arc<Grant>,
    environments: tilo_workspace_service::environment::EnvironmentRegistry,
    device: [u8; 32],
}

impl Drop for Present {
    fn drop(&mut self) {
        self.environments.departed(self.device);
    }
}

impl SessionAuthorization for Present {
    fn publication(&self) -> Option<&str> {
        self.grant.publication()
    }

    fn subscriptions(&self) -> &[String] {
        self.grant.subscriptions()
    }

    fn subscription_prefix(&self) -> Option<&str> {
        self.grant.subscription_prefix()
    }

    fn current(&self, direction: Direction, namespace: &str) -> bool {
        self.grant.current(direction, namespace)
    }

    fn changed(&self) -> Option<watch::Receiver<u64>> {
        self.grant.changed()
    }

    fn call(&self, direction: Direction, call: &str) -> bool {
        self.grant.call(direction, call)
    }
}

/// Who a device's grant is for: the device, in one workspace.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct DeviceKey {
    device: [u8; 32],
    workspace: WorkspaceId,
}

/// Every device the live origin admits over iroh, by the device and workspace it
/// presented.
type Members = BTreeMap<DeviceKey, Arc<Grant>>;

/// The live origin's authority: the devices the workspaces' governance admits,
/// and nothing else — a device that never opened a live session is
/// nobody here.
pub(crate) struct Authority {
    server: ServerProtocol,
    /// The devices this host certified for its browser environments: they
    /// publish their window's page and are offered nobody else's tracks.
    environments: tilo_workspace_service::environment::EnvironmentRegistry,
    members: Mutex<Members>,
}

impl Authority {
    pub(crate) fn new(server: ServerProtocol) -> Self {
        let environments = server.environments.clone();
        Self {
            server,
            environments,
            members: Mutex::new(Members::new()),
        }
    }

    /// Who the live origin carries now: every person with an admitted device,
    /// with the devices they are here on. One person on two devices is one
    /// entry. The live origin is the one account of who is here: a native device and a
    /// page alike reach it, and nothing counts a page that never joined.
    pub(crate) fn devices(&self) -> BTreeMap<[u8; 32], BTreeSet<[u8; 32]>> {
        let mut people: BTreeMap<[u8; 32], BTreeSet<[u8; 32]>> = BTreeMap::new();
        for grant in self.carried() {
            if grant.admitted() {
                people
                    .entry(grant.peer.user_public_key)
                    .or_default()
                    .insert(grant.peer.device_public_key);
            }
        }
        people
    }

    /// Every grant the live origin carries, as the one account of who is here.
    fn carried(&self) -> Vec<Arc<Grant>> {
        self.members.lock_recover().values().cloned().collect()
    }

    /// Re-answer every device: does its workspace admit its device now?
    /// Called on each relay-wide authority change, never on a timer.
    async fn refresh(&self) {
        let members: Vec<Arc<Grant>> = self.members.lock_recover().values().cloned().collect();
        let mut calls: BTreeMap<WorkspaceId, Arc<Calls>> = BTreeMap::new();
        for grant in members {
            let admitted = self
                .admits_as(&grant.peer, grant.workspace, WorkspaceOperation::Publish)
                .await;
            grant.set(admitted);
            let of_workspace = if let Some(read) = calls.get(&grant.workspace) {
                Arc::clone(read)
            } else {
                let read = self.calls(grant.workspace).await;
                calls.insert(grant.workspace, Arc::clone(&read));
                read
            };
            grant.set_calls(of_workspace);
        }
    }

    /// What each call of `workspace` allows now, by the tag its tracks name:
    /// none for a workspace this relay holds no copy of, whose calls then
    /// carry as their channels allow by default.
    async fn calls(&self, workspace: WorkspaceId) -> Arc<Calls> {
        let Some(workspaces) = self.server.admin.get() else {
            return Arc::default();
        };
        let copy = if let Some(copy) = workspaces.hosted_copy(workspace).await {
            Some(copy)
        } else {
            workspaces.member_copy(workspace).await
        };
        let Some((_, peer)) = copy else {
            return Arc::default();
        };
        Arc::new(
            peer.governance()
                .calls()
                .into_iter()
                .map(|(channel, rule)| (collab_types::calls::tag(&channel), rule))
                .collect(),
        )
    }

    /// Whether the workspace's governance admits `peer`'s device to `op` now:
    /// the same answer every live session of the workspace takes, read from the
    /// workspace as it stands.
    async fn admits_as(
        &self,
        peer: &AuthenticatedPeer,
        workspace: WorkspaceId,
        op: WorkspaceOperation,
    ) -> bool {
        let workspaces = self.server.workspaces.lock().await;
        let Some(workspace) = workspaces.get(&workspace).cloned() else {
            return false;
        };
        drop(workspaces);
        let runtime = workspace.lock().await;
        workspace_access::decide(&runtime, &self.server.trust(), peer, op).admitted()
    }

    /// Admit the device that reached an iroh connection, which proves itself
    /// with the certificate its grant carries.
    async fn admit_member(
        &self,
        endpoint: [u8; 32],
        proof: &[u8],
    ) -> Result<Arc<dyn SessionAuthorization>, Denied> {
        let grant = tilo_live::grant::Native::parse(proof).ok_or(Denied)?;
        let workspace = tilo_live::live::workspace_id(grant.workspace);
        let id = WorkspaceId::parse(&workspace).ok_or(Denied)?;
        if grant.scope != Scope::workspace(&workspace).id() {
            return Err(Denied);
        }
        // The certificate binds the handshake's key to the device: a device
        // presenting another device's certificate is refused.
        let certificate: tilo_user_identity::DeviceCert =
            serde_json::from_slice(&grant.certificate).map_err(|_| Denied)?;
        // The endpoint speaks for that device: it is the device's own key, or
        // one the device signed a live delegation for — a page's key cannot
        // leave its ownership, so its endpoint speaks under a key of its own.
        if endpoint != certificate.device_public_key {
            let delegation: tilo_user_identity::EndpointDelegation =
                serde_json::from_slice(&grant.delegation).map_err(|_| Denied)?;
            if delegation.device_public_key != certificate.device_public_key {
                return Err(Denied);
            }
            delegation
                .verify(&endpoint, tilo_clock::unix_now_ns())
                .map_err(|_| Denied)?;
        }
        let device = certificate.device_public_key;
        if certificate.is_expired_at(tilo_clock::unix_now_ns()) {
            return Err(Denied);
        }
        let peer = AuthenticatedPeer {
            user_public_key: certificate.user_public_key,
            device_public_key: device,
            cert: certificate,
        };
        // A device dials the live origin as it enters, while the workspace is still
        // taking it in: the admission waits for the workspace and decides on
        // it, so a newcomer is not refused for arriving at a busy moment.
        if !self.admits_as(&peer, id, WorkspaceOperation::Publish).await {
            return Err(Denied);
        }
        let key = DeviceKey {
            device,
            workspace: id,
        };
        // An environment's session in a workspace's VM joins the workspace and
        // then takes part over its own direct link and this live origin; it never
        // opens a state-protocol session. So the live origin it publishes its window on
        // is the lasting connection that says it is still here, and a stopped
        // environment is not mistaken for one whose connection dropped.
        let environment =
            tilo_workspace_service::environment::environment_of(&self.environments, &device)
                .is_some();
        let admitted = self.members.lock_recover().get(&key).cloned();
        let member = if let Some(member) = admitted {
            member.set(true);
            member
        } else {
            let member = Arc::new(Grant::new(id, &workspace, peer, environment));
            self.members.lock_recover().insert(key, member.clone());
            member
        };
        member.set_calls(self.calls(id).await);
        if !environment {
            return Ok(member);
        }
        self.environments.arrived(device);
        Ok(Arc::new(Present {
            grant: member,
            environments: self.environments.clone(),
            device,
        }))
    }
}

impl RelayAuthority for Authority {
    fn admit<'a>(&'a self, peer: Peer, proof: &'a [u8]) -> Admission<'a> {
        // Every device reaches this live origin over iroh, a page included: it
        // proves itself with the certificate its grant carries.
        let Peer(device) = peer;
        Box::pin(self.admit_member(device, proof))
    }
}

/// The live origin as a listener serves it: who is admitted, and the relay the admitted
/// copy into. Cloning shares the one origin.
#[derive(Clone)]
pub(crate) struct Served {
    authority: Arc<Authority>,
    relay: Relay,
}

impl Served {
    /// Who the live origin carries: the one account of who is here.
    pub(crate) fn authority(&self) -> Arc<Authority> {
        Arc::clone(&self.authority)
    }

    /// The relay an admitted session copies into.
    pub(crate) fn relay(&self) -> Relay {
        self.relay.clone()
    }
}

/// The relay's live origin: the origin every carrier of its workspaces'
/// tracks reaches, and the authority that admits them.
pub(crate) struct LiveOrigin {
    authority: Arc<Authority>,
    relay: Relay,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl LiveOrigin {
    /// Start the live origin for `server`'s workspace: the origin its sessions copy
    /// into, and the answer that cuts an admitted one when the workspace moves
    /// its membership. Must be called inside a Tokio runtime.
    pub(crate) fn start(server: ServerProtocol) -> Self {
        let authority = Arc::new(Authority::new(server));
        let origin = Relay::new(authority.clone());
        let refreshing = authority.clone();
        let refresh = tokio::spawn(async move {
            let mut epoch = tilo_workspace_service::authority_epoch::subscribe();
            while epoch.changed().await.is_ok() {
                refreshing.refresh().await;
            }
        });
        Self {
            authority,
            relay: origin,
            tasks: vec![refresh],
        }
    }

    /// The protocol a device dials this live origin on: one `moq-lite` session on
    /// the iroh carrier, admitted by the device's grant as any other carrier
    /// session is.
    pub(crate) fn protocol(&self) -> (Vec<u8>, Box<dyn iroh::protocol::DynProtocolHandler>) {
        let relay = self.relay.clone();
        let handler = tilo_live::native::handler(move |session: tilo_live::native::Session| {
            let relay = relay.clone();
            // A session this live origin admitted may open what an admitted session opens.
            async move { tilo_live::native::serve_on(&relay, session).await }
        });
        (tilo_live::native::ALPN.to_vec(), Box::new(handler))
    }

    /// The live origin's origin, which every carrier's sessions are served into.
    pub(crate) fn origin(&self) -> Relay {
        self.relay.clone()
    }

    /// This live origin as a browser listener serves it, and as the listener is served
    /// again after it stopped: cloning shares the one origin.
    pub(crate) fn served(&self) -> Served {
        Served {
            authority: Arc::clone(&self.authority),
            relay: self.relay.clone(),
        }
    }

    /// The one task the live origin runs besides its sessions, which stands while
    /// the live origin does.
    pub(crate) fn tasks(&mut self) -> Vec<tokio::task::JoinHandle<()>> {
        std::mem::take(&mut self.tasks)
    }
}

/// Serve one WebTransport session at the live origin's path: the device proves
/// itself with the relay handshake on the session's first stream (the
/// transport does not say who dialled it, as iroh's does), then the
/// session is the live origin's, admitted by the device's grant as any other
/// carrier's.
pub(crate) async fn serve_web(
    origin: &Relay,
    protocol: &ServerProtocol,
    session: tilo_net::webtransport::Session,
) {
    let link = tilo_server_client::link::Link::web(session.clone());
    let Ok((mut send, mut recv)) = link.accept_bi().await else {
        return;
    };
    let Ok(device) = protocol.authenticate_member(&mut recv, &mut send).await else {
        link.close("the device did not prove itself");
        return;
    };
    send.finish();
    origin
        .serve_session(session, Peer(device.device_public_key))
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A device of the person who owns the workspace, as the live origin admits one.
    fn peer(device: [u8; 32]) -> AuthenticatedPeer {
        AuthenticatedPeer {
            user_public_key: [0xA1; 32],
            device_public_key: device,
            cert: tilo_user_identity::DeviceCert {
                user_public_key: [0xA1; 32],
                device_public_key: device,
                device_label: "Browser".into(),
                capabilities: tilo_user_identity::DeviceCapabilities::member(),
                issued_at_ns: 0,
                expires_at_ns: None,
                signature: [0; 64],
                issuer: None,
            },
        }
    }

    fn names(device: [u8; 32]) -> Namespace {
        Namespace::new(&Scope::workspace("ws-1"), device)
    }

    tilo_test_env::scenario! {
        fn a_grant_names_what_its_holder_publishes_and_hears;

        phase a_member_hears_every_device_of_its_workspace;
        {
            let grant = Grant::new(WorkspaceId::from_u128(7), "ws-1", peer([1; 32]), false);
            assert_eq!(grant.subscription_prefix(), Some(grant.scope.as_str()));
            assert!(grant.current(Direction::Subscribe, &names([2; 32]).path()));
            assert!(grant.current(Direction::Publish, &names([1; 32]).path()));
            assert!(
                !grant.current(Direction::Publish, &names([2; 32]).path()),
                "nobody publishes in another device's name"
            );
        }

        phase a_browser_environment_publishes_its_window_and_hears_nobody;
        {
            let grant = Grant::new(WorkspaceId::from_u128(7), "ws-1", peer([1; 32]), true);
            assert_eq!(
                grant.publication(),
                Some(names([1; 32]).path().as_str()),
                "it publishes its own window's page"
            );
            assert!(grant.subscriptions().is_empty());
            assert_eq!(
                grant.subscription_prefix(),
                None,
                "no track of the workspace is offered to it: no call, camera, screen or other window"
            );
            assert!(!grant.current(Direction::Subscribe, &names([2; 32]).path()));
            assert!(!grant.current(Direction::Subscribe, &names([1; 32]).path()));
            assert!(grant.current(Direction::Publish, &names([1; 32]).path()));
        }
    }
}
