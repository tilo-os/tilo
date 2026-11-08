//! The relay's live plane: one `moq-lite` relay every carrier of a workspace's
//! live tracks reaches, and the authority that admits them to it.
//!
//! The plane belongs to the relay, not to a listener: every participant dials
//! it over the relay's iroh endpoint ([`tilo_live::native`]) as its own
//! certificate says it may. A page dials it the same way: its device
//! certificate is the grant, and the delegation it carries names the endpoint
//! key of its page, which cannot leave custody. Either way the workspace's
//! governance answers for it as it answers for any member, and the answer
//! stays current from the relay-wide authority epoch — a device removed from
//! the workspace is cut on that event, not on a timer.
//!
//! The sessions themselves are `tilo-live`'s: this module answers who may open
//! one, and [`Relay::serve_session`] runs the same admission preface,
//! `moq-lite` session and copying relay for every carrier.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tilo_live::live::Namespace;
use tilo_live::relay::{Denied, Direction, Peer, Relay, RelayAuthority, SessionAuthorization};
use tilo_live::scope::Scope;
use tilo_lock::MutexRecover as _;
use tokio::sync::watch;

use crate::auth::AuthenticatedPeer;
use crate::host::RelayProtocol;
use crate::workspace_access::{self, WorkspaceOperation};

/// One participant's live session: whose it is, the scope it publishes in, and
/// whether the workspace still admits it.
struct Grant {
    workspace: u64,
    peer: AuthenticatedPeer,
    /// The namespace the participant publishes its own tracks under: one
    /// participant of the workspace scope, as every device's is.
    namespace: String,
    /// The subtree every participant of the same scope publishes in.
    scope: String,
    admitted: AtomicBool,
    /// Reports each change of `admitted`: the relay re-observes on it and cuts
    /// a session whose answer narrowed.
    changed: watch::Sender<u64>,
}

impl Grant {
    fn new(workspace: u64, workspace_name: &str, peer: AuthenticatedPeer) -> Self {
        let namespace = Namespace::new(&Scope::workspace(workspace_name), peer.device_public_key);
        Self {
            workspace,
            peer,
            scope: namespace.scope_path(),
            namespace: namespace.path(),
            admitted: AtomicBool::new(true),
            changed: watch::channel(0_u64).0,
        }
    }

    /// Whether the workspace admits this participant now.
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
        Some(&self.scope)
    }

    fn current(&self, direction: Direction, namespace: &str) -> bool {
        self.admitted()
            && match direction {
                Direction::Publish => namespace == self.namespace,
                Direction::Subscribe => namespace
                    .strip_prefix(&self.scope)
                    .is_some_and(|rest| rest.starts_with("/participant/")),
            }
    }

    fn changed(&self) -> Option<watch::Receiver<u64>> {
        Some(self.changed.subscribe())
    }
}

/// Who a device's grant is for: the device, in one workspace.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct DeviceKey {
    device: [u8; 32],
    workspace: u64,
}

/// Every device the plane admits over iroh, by the device and workspace it
/// presented.
type Members = BTreeMap<DeviceKey, Arc<Grant>>;

/// The live plane's authority: the devices the workspaces' governance admits,
/// and nothing else — a participant that never opened a plane session is
/// nobody here.
pub(crate) struct Authority {
    relay: RelayProtocol,
    members: Mutex<Members>,
}

impl Authority {
    pub(crate) fn new(relay: RelayProtocol) -> Self {
        Self {
            relay,
            members: Mutex::new(Members::new()),
        }
    }

    /// Who the plane carries now: every person with an admitted participant,
    /// with the devices they are here on. One person on two devices is one
    /// entry. A plane is the one account of who is here: a native device and a
    /// page alike reach it, and nothing counts a page that never joined.
    pub(crate) fn participants(&self) -> BTreeMap<[u8; 32], BTreeSet<[u8; 32]>> {
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

    /// Every grant the plane carries, as the one account of who is here.
    fn carried(&self) -> Vec<Arc<Grant>> {
        self.members.lock_recover().values().cloned().collect()
    }

    /// The devices the plane carries in `workspace` now, each with the person
    /// it is certified as: who a page there pairs with before anyone speaks.
    pub(crate) fn persons_in(&self, workspace: u64) -> Vec<([u8; 32], [u8; 32])> {
        let mut persons: Vec<([u8; 32], [u8; 32])> = self
            .carried()
            .into_iter()
            .filter(|grant| grant.admitted() && grant.workspace == workspace)
            .map(|grant| (grant.peer.device_public_key, grant.peer.user_public_key))
            .collect();
        persons.sort_unstable();
        persons.dedup();
        persons
    }

    /// Whether the workspace lets `peer`s device be a page in its plane now: a
    /// page asks who is here over its session, which no session token names, so
    /// the answer comes from the workspace itself.
    pub(crate) async fn admits_page(&self, peer: &AuthenticatedPeer, workspace: u64) -> bool {
        self.admits_as(peer, workspace, WorkspaceOperation::Live)
            .await
    }

    /// Re-answer every participant: does its workspace admit its device now?
    /// Called on each relay-wide authority change, never on a timer.
    async fn refresh(&self) {
        let members: Vec<Arc<Grant>> = self.members.lock_recover().values().cloned().collect();
        for grant in members {
            let admitted = self
                .admits_as(&grant.peer, grant.workspace, WorkspaceOperation::Publish)
                .await;
            grant.set(admitted);
        }
    }

    /// Whether the workspace's governance admits `peer`'s device to `op` now:
    /// the same answer every plane of the workspace takes, read from the
    /// workspace as it stands.
    async fn admits_as(
        &self,
        peer: &AuthenticatedPeer,
        workspace: u64,
        op: WorkspaceOperation,
    ) -> bool {
        let workspaces = self.relay.workspaces.lock().await;
        let Some(workspace) = workspaces.get(&workspace).cloned() else {
            return false;
        };
        drop(workspaces);
        let runtime = workspace.lock().await;
        workspace_access::decide(&runtime, &self.relay.trust(), peer, op).admitted()
    }

    /// [`Self::admits_as`] for the carrier's own admission, which the relay
    /// runs on a task that cannot wait for the workspace: a busy lock admits
    /// nobody, and the device redials once the workspace has answered.
    fn admits_now(&self, peer: &AuthenticatedPeer, workspace: u64, op: WorkspaceOperation) -> bool {
        let Ok(workspaces) = self.relay.workspaces.try_lock() else {
            return false;
        };
        let Some(workspace) = workspaces.get(&workspace).cloned() else {
            return false;
        };
        drop(workspaces);
        let Ok(runtime) = workspace.try_lock() else {
            return false;
        };
        workspace_access::decide(&runtime, &self.relay.trust(), peer, op).admitted()
    }

    /// Admit the device that reached an iroh connection, which proves itself
    /// with the certificate its grant carries.
    fn admit_member(
        &self,
        endpoint: [u8; 32],
        proof: &[u8],
    ) -> Result<Arc<dyn SessionAuthorization>, Denied> {
        let grant = tilo_live::grant::Native::parse(proof).ok_or(Denied)?;
        let workspace = tilo_live::live::workspace_id(grant.workspace);
        let id = tilo_relay_protocol::room_workspace_id(&workspace).map_err(|_| Denied)?;
        if grant.scope != Scope::workspace(&workspace).id() {
            return Err(Denied);
        }
        // The certificate binds the handshake's key to the device: a device
        // presenting another device's certificate is refused.
        let certificate: tilo_user_identity::DeviceCert =
            serde_json::from_slice(&grant.certificate).map_err(|_| Denied)?;
        // The endpoint speaks for that device: it is the device's own key, or
        // one the device signed a live delegation for — a page's key cannot
        // leave its custody, so its endpoint speaks under a key of its own.
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
        if !self.admits_now(&peer, id, WorkspaceOperation::Publish) {
            return Err(Denied);
        }
        let key = DeviceKey {
            device,
            workspace: id,
        };
        let admitted = self.members.lock_recover().get(&key).cloned();
        let member = if let Some(member) = admitted {
            member.set(true);
            member
        } else {
            let member = Arc::new(Grant::new(id, &workspace, peer));
            self.members.lock_recover().insert(key, member.clone());
            member
        };
        Ok(member)
    }
}

impl RelayAuthority for Authority {
    fn admit(&self, peer: Peer, proof: &[u8]) -> Result<Arc<dyn SessionAuthorization>, Denied> {
        // Every participant reaches this plane over iroh, a page included: it
        // proves itself with the certificate its grant carries.
        let Peer::Native(device) = peer else {
            return Err(Denied);
        };
        self.admit_member(device, proof)
    }
}

/// The relay's live plane: the origin every carrier of its workspaces'
/// tracks reaches, and the authority that admits them.
pub(crate) struct Plane {
    authority: Arc<Authority>,
    relay: Relay,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl Plane {
    /// Start the plane for `relay`'s workspace: the origin its sessions copy
    /// into, and the answer that cuts an admitted one when the workspace moves
    /// its membership. Must be called inside a Tokio runtime.
    pub(crate) fn start(relay: RelayProtocol) -> Self {
        let authority = Arc::new(Authority::new(relay));
        let plane = Relay::new(authority.clone());
        let refreshing = authority.clone();
        let refresh = tokio::spawn(async move {
            let mut epoch = crate::authority_epoch::subscribe();
            while epoch.changed().await.is_ok() {
                refreshing.refresh().await;
            }
        });
        Self {
            authority,
            relay: plane,
            tasks: vec![refresh],
        }
    }

    /// The protocol a device dials this plane on: one `moq-lite` session on
    /// the iroh carrier, admitted by the device's grant as any other carrier
    /// session is.
    pub(crate) fn protocol(&self) -> (Vec<u8>, Box<dyn iroh::protocol::DynProtocolHandler>) {
        let relay = self.relay.clone();
        let handler = tilo_live::native::handler(move |session: tilo_live::native::Session| {
            let relay = relay.clone();
            async move {
                let peer = Peer::Native(session.remote());
                relay
                    .serve_session(tilo_live::relay::VoiceRoute::negotiated(), session, peer)
                    .await;
            }
        });
        (tilo_live::native::ALPN.to_vec(), Box::new(handler))
    }

    /// Who the plane carries: the one account of who is here.
    pub(crate) fn authority(&self) -> Arc<Authority> {
        Arc::clone(&self.authority)
    }

    /// The one task the plane runs besides its sessions, which stands while
    /// the plane does.
    pub(crate) fn tasks(&mut self) -> Vec<tokio::task::JoinHandle<()>> {
        std::mem::take(&mut self.tasks)
    }
}
