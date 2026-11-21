//! Every workspace this profile takes part in, as the device's workspace
//! service holds it.
//!
//! One service holds every workspace: the profile's own, which it governs and
//! the runtime's apps write into through it, and the member copies of the
//! workspaces other devices serve, which this device joined. The runtime keeps
//! no replica of its own: what it reads — a workspace's name and governance,
//! the person the device acts for there, the people holding its keys — it reads
//! from the service, and what it writes it commits there.
//!
//! The workspaces the desktop session governs are refused here: the session
//! is their one writer, and this runtime takes no part in them.

use std::{
    collections::BTreeSet,
    sync::{Arc, Mutex, OnceLock},
};

use tilo_desktop_model::{Workspace, WorkspaceId};
use tilo_identity::identity_store::DeviceIdentity;
use tilo_peer_state::StatePeer;
use tilo_peer_state::actions::{ReadFuture, Workspaces as PeerWorkspaces};
use tilo_workspace_service::apps::document_content::RouteContent;
use tilo_workspace_service::service::WorkspaceService;
use tilo_workspace_service::workspaces::{SessionAdmission, Workspaces};

/// Every workspace this profile takes part in, through the workspace service
/// this device runs.
pub(super) struct ServiceWorkspaces {
    service: Arc<WorkspaceService>,
    identity: DeviceIdentity,
    endpoint: iroh::Endpoint,
    /// Workspaces this profile must not touch, whose writes are refused here:
    /// those the desktop session governs.
    refused: BTreeSet<WorkspaceId>,
    /// The record content service of these workspaces, once it is made.
    content: OnceLock<Arc<RouteContent>>,
    /// What the service hands the content its members answer this device with,
    /// held here so it is there for as long as the runtime.
    keepers: Mutex<Vec<Arc<dyn tilo_workspace_service::workspaces::ContentKeeper>>>,
}

impl ServiceWorkspaces {
    pub(super) fn new(
        service: Arc<WorkspaceService>,
        identity: DeviceIdentity,
        endpoint: iroh::Endpoint,
        refused: BTreeSet<WorkspaceId>,
    ) -> Arc<Self> {
        Arc::new(Self {
            service,
            identity,
            endpoint,
            refused,
            content: OnceLock::new(),
            keepers: Mutex::new(Vec::new()),
        })
    }

    /// The workspace service this device runs.
    pub(super) fn service(&self) -> &Arc<WorkspaceService> {
        &self.service
    }

    /// The service's workspaces, as this device holds them.
    pub(super) fn copies(&self) -> &Workspaces {
        self.service.workspaces()
    }

    /// This device's endpoint, which the profile's other services share.
    pub(super) fn endpoint(&self) -> iroh::Endpoint {
        self.endpoint.clone()
    }

    /// What signs as this device, as its action service signs every request it
    /// makes.
    pub(super) fn signer(&self) -> Arc<dyn collab_types::signer::DeviceSigner> {
        Arc::clone(&self.identity.signer)
    }

    /// The device certificate this profile's ownership holds.
    pub(super) fn device_cert(&self) -> &tilo_user_identity::DeviceCert {
        &self.identity.device_cert
    }

    /// The person this device belongs to: whose devices may ask it for work.
    pub(super) fn operator(&self) -> [u8; 32] {
        self.identity.device_cert.user_public_key
    }

    /// Told each time this profile starts holding a workspace as a member.
    pub(super) fn entered(&self) -> Arc<tokio::sync::Notify> {
        self.copies().members_held()
    }

    /// Whether this profile must not touch `workspace`: the desktop session
    /// governs it.
    pub(super) fn refuses(&self, workspace: WorkspaceId) -> bool {
        self.refused.contains(&workspace)
    }

    /// The workspaces this profile must not touch.
    pub(super) fn refused_workspaces(&self) -> BTreeSet<WorkspaceId> {
        self.refused.clone()
    }

    /// Whether this profile holds `workspace` as a member copy.
    pub(super) fn holds(&self, workspace: WorkspaceId) -> bool {
        let copies = self.copies().clone();
        block_on_owned(async move { copies.member_ids().await.contains(&workspace) })
    }

    /// Whether this profile holds no member copy at all.
    pub(super) fn is_empty(&self) -> bool {
        let copies = self.copies().clone();
        block_on_owned(async move { copies.member_ids().await.is_empty() })
    }

    /// `workspace`'s replica as this device holds it: its member copy, or the
    /// workspace the service governs here.
    pub(super) fn peer(&self, workspace: WorkspaceId) -> Option<StatePeer> {
        let copies = self.copies().clone();
        block_on_owned(async move {
            match copies.member_copy(workspace).await {
                Some((_, peer)) => Some(peer),
                None => copies.governed_copy(workspace).await.map(|(_, peer)| peer),
            }
        })
    }

    /// The member copies this device holds, each with the name its governance
    /// gives it and the replica it keeps.
    pub(super) fn held(&self) -> Vec<(WorkspaceId, String, StatePeer)> {
        let workspaces = self.copies().clone();
        block_on_owned(async move {
            let mut held = Vec::new();
            for workspace in workspaces.member_ids().await {
                if let Some((name, peer)) = workspaces.member_copy(workspace).await {
                    held.push((workspace, name, peer));
                }
            }
            held
        })
    }

    /// Every workspace this profile holds as a member copy, as a workspace
    /// its governance describes.
    pub(super) fn workspaces(&self) -> Vec<Workspace> {
        self.held()
            .into_iter()
            .filter_map(|(workspace, name, peer)| describe(workspace, &name, &peer))
            .collect()
    }

    /// One member copy as [`Self::workspaces`] describes it, built alone.
    pub(super) fn workspace(&self, workspace: WorkspaceId) -> Option<Workspace> {
        let copies = self.copies().clone();
        block_on_owned(async move {
            let (name, peer) = copies.member_copy(workspace).await?;
            describe(workspace, &name, &peer)
        })
    }

    /// What this device is to `workspace`, for the workspace's Files endpoint:
    /// the certificate the workspace holds for this device's key.
    ///
    /// # Errors
    /// `workspace` is no workspace id, or the certificate is not valid now.
    pub(super) fn files_credentials(
        &self,
        workspace: &str,
    ) -> std::io::Result<tilo_fs::identity::PeerCredentials> {
        let workspace = WorkspaceId::parse(workspace)
            .ok_or_else(|| std::io::Error::other("not a workspace id"))?;
        if !self.holds(workspace) {
            return Err(std::io::Error::other(
                "the workspace is not held here as a member copy",
            ));
        }
        let (cert, _) = self.identity.material_for(workspace);
        tilo_fs::identity::PeerCredentials::certified(
            cert,
            self.signer(),
            tilo_clock::unix_now_ns(),
        )
        .map_err(std::io::Error::other)
    }

    /// Select only a member copy this device's actor currently takes part in,
    /// as that person.
    ///
    /// # Errors
    /// The workspace is no member copy here, or `actor` is not the person this
    /// device acts for in it.
    pub(super) fn action_workspace(&self, actor: &str, workspace: &str) -> Result<String, String> {
        let workspace = WorkspaceId::parse(workspace).ok_or("invalid MCP workspace")?;
        if self.refuses(workspace) {
            return Err(
                "this device takes part in that workspace through the desktop session".into(),
            );
        }
        let copies = self.copies().clone();
        let (_, peer) = block_on_owned(async move { copies.member_copy(workspace).await })
            .ok_or("no such workspace held")?;
        let admitted = peer
            .permissions(
                self.identity.device_cert.device_public_key,
                tilo_clock::wall(),
            )
            .map_err(|error| error.to_string())?
            .person_id()
            .ok_or("the device acts for no person")?
            .to_owned();
        if actor != admitted {
            return Err(
                "the selected profile actor is not this workspace's admitted identity".into(),
            );
        }
        Ok(workspace.to_string())
    }

    /// Keep `commit`, this device's own membership change (a lease it claims),
    /// in the member copy `workspace` and send it to the device serving it.
    /// Linux only: the VM leases it claims are the hosted-VM service's.
    #[cfg(target_os = "linux")]
    pub(super) async fn claim_lease(
        &self,
        workspace: WorkspaceId,
        id: String,
        ttl: std::time::Duration,
        rank: neighbour_wire::placement::Rank,
    ) -> Result<(), String> {
        let command = tilo_peer_state::workspace_membership::WorkspaceMembershipCommand::Lease {
            id,
            ttl_ms: u64::try_from(ttl.as_millis()).unwrap_or(u64::MAX),
            rank,
        };
        let commit = self
            .copies()
            .member_membership_write(&workspace.to_string(), command)
            .await?;
        self.copies()
            .commit_member_document(&workspace.to_string(), commit)
            .await?;
        Ok(())
    }

    /// Admit a run of an agent in `workspace`, as `actor`, pinned to the
    /// workspace's material now.
    ///
    /// # Errors
    /// The workspace is held neither here nor as a member copy, or the actor
    /// is not the person this device acts for in it.
    pub(super) async fn admit_execution(
        &self,
        workspace: &str,
        actor: &str,
    ) -> Result<Arc<SessionAdmission>, String> {
        let cert = &self.identity.device_cert;
        let join = tilo_identity_client::collaboration_join_digest(workspace)
            .map_err(|error| error.to_string())?;
        let presentation = runtime_collab::CertifiedJoinIdentity {
            workspace_id: workspace.into(),
            device_certificate: serde_json::to_vec(cert).map_err(|error| error.to_string())?,
            proof: collab_types::signer::sign_now(self.identity.signer.as_ref(), join.to_vec())
                .map_err(|error| error.to_string())?
                .to_vec(),
        }
        .encode()
        .map_err(|error| error.to_string())?;
        self.copies()
            .admit_execution(workspace, actor, cert, presentation)
            .await
    }

    /// Re-check every run admitted to `workspace`.
    pub(super) fn recheck_executions(&self, workspace: WorkspaceId) -> Result<(), String> {
        self.copies().recheck_executions(workspace)
    }

    /// Ask the workspaces' other devices, through this device's own workspace
    /// host, whether they would run an offloadable action sooner.
    pub(super) fn offer_action_service(
        &self,
    ) -> Option<(
        tilo_peer_state::actions::ActionService,
        iroh::Endpoint,
        tokio::runtime::Handle,
    )> {
        let runtime = tilo_rt::try_app().ok()?.handle().clone();
        let reads = Arc::new(Reads {
            service: Arc::clone(&self.service),
            entering: None,
        });
        let action_service = {
            let _entered = runtime.enter();
            tilo_peer_state::actions::ActionService::over(
                self.signer(),
                Arc::new(tilo_peer_state::actions::NoActions),
                reads,
            )
        };
        Some((action_service, self.endpoint(), runtime))
    }

    /// Hand every write of another device the workspaces held here take in
    /// to the apps following the workspace's records, on the brokers and
    /// `fanout`.
    pub(super) fn follow_admitted_writes(
        &self,
        brokers: Arc<Mutex<tilo_workspace_service::apps::collab_core::BrokerChannels>>,
        fanout: Arc<tilo_workspace_service::apps::collab_broker::FanoutState>,
    ) {
        tilo_rt::app().spawn(
            tilo_workspace_service::apps::collab_inbound::follow_admitted_writes(
                self.copies(),
                move |document| {
                    tilo_workspace_service::apps::collab_inbound::deliver(
                        &brokers, &fanout, document,
                    );
                },
                || {},
            ),
        );
    }

    /// The record content service of these workspaces, once it is made.
    pub(super) fn record_content_service(&self) -> Option<Arc<RouteContent>> {
        self.content.get().cloned()
    }

    /// Keep `content` as this profile's record content service from now on.
    pub(super) fn install_content(&self, content: &Arc<RouteContent>) {
        let _ = self.content.set(Arc::clone(content));
    }

    /// Keep `keeper`, which the service hands the content its members answer
    /// this device with.
    pub(super) fn install_keeper(
        &self,
        keeper: Arc<dyn tilo_workspace_service::workspaces::ContentKeeper>,
    ) {
        if let Ok(mut keepers) = self.keepers.lock() {
            keepers.push(keeper);
        }
    }
}

/// A workspace as the member copy's governance describes it.
fn describe(workspace: WorkspaceId, name: &str, peer: &StatePeer) -> Option<Workspace> {
    let document = peer.admitted_workspace()?;
    let mut described = Workspace::new(workspace, name.to_owned());
    tilo_peer_state::workspace::adopt_workspace(&mut described, document, &[]);
    described.sandbox = peer.governance().governed_sandbox().ok()?;
    // The names the workspace's governance keeps for its people: what every surface showing
    // someone who is not here reads, from the same list the agent worker's members come from.
    if let Ok(people) = peer.governance().governed_members(tilo_clock::wall()) {
        described.named_people(
            people
                .members
                .iter()
                .filter(|member| !member.removed)
                .filter_map(|member| Some((member.id.person_id()?, member.name.as_str()))),
        );
    }
    Some(described)
}

impl crate::apps::LaunchGrantsSource for ServiceWorkspaces {
    /// What a launch into `workspace` hands its apps: the sandbox its
    /// governance records, whether this device governs it or holds it as a
    /// member copy.
    fn launch_grants(&self, workspace: &str) -> collab_types::caps::LaunchGrants {
        let Some(id) = WorkspaceId::parse(workspace) else {
            return collab_types::caps::LaunchGrants::default();
        };
        let Some(sandbox) = self
            .peer(id)
            .and_then(|peer| peer.governance().governed_sandbox().ok())
        else {
            return collab_types::caps::LaunchGrants::default();
        };
        let mut held = Workspace::new(id, String::new());
        held.sandbox = sandbox;
        held.launch_grants()
    }
}

/// The workspace replicas the runtime's own services read: the profile's own
/// workspace through the service that governs it, and each member copy.
pub(super) struct Reads {
    pub(super) service: Arc<WorkspaceService>,
    pub(super) entering: Option<Arc<super::take_work::Entering>>,
}

impl PeerWorkspaces for Reads {
    fn read<'a>(
        &'a self,
        workspace: &'a str,
        read: &'a mut (dyn FnMut(&StatePeer) + Send),
    ) -> ReadFuture<'a> {
        Box::pin(async move { self.read_now(workspace, read).await })
    }

    fn read_for<'a>(
        &'a self,
        asker: [u8; 32],
        workspace: &'a str,
        read: &'a mut (dyn FnMut(&StatePeer) + Send),
    ) -> ReadFuture<'a> {
        let entering = self
            .entering
            .as_ref()
            .filter(|enter| enter.server() == asker)
            .cloned();
        Box::pin(async move {
            if self.read_now(workspace, &mut *read).await {
                return true;
            }
            let Some(entering) = entering else {
                return false;
            };
            if let Err(reason) = entering.enter(workspace).await {
                tracing::debug!(%workspace, %reason, "a workspace's request is refused here");
                return false;
            }
            self.read_now(workspace, &mut *read).await
        })
    }
}

impl Reads {
    async fn read_now(&self, workspace: &str, read: &mut (dyn FnMut(&StatePeer) + Send)) -> bool {
        let Some(id) = WorkspaceId::parse(workspace) else {
            return false;
        };
        let workspaces = self.service.workspaces();
        let copy = match workspaces.member_copy(id).await {
            Some((_, peer)) => Some(peer),
            None => workspaces.governed_copy(id).await.map(|(_, peer)| peer),
        };
        let Some(peer) = copy else {
            return false;
        };
        read(&peer);
        true
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::significant_drop_tightening,
    reason = "a test fixture failing is its test failing, and its service lives as long as the fixture"
)]
impl ServiceWorkspaces {
    /// A service governing `workspace` alone, on a profile of its own, with a
    /// synthetic device its governor signed: what a test of the runtime's own
    /// workspace starts from.
    pub(in crate::standalone) async fn test_hosting(
        workspace: WorkspaceId,
    ) -> (Arc<Self>, tempfile::TempDir) {
        use ed25519_dalek::SigningKey;
        use tilo_user_identity::{DeviceCapabilities, DeviceCert};

        let directory = tempfile::tempdir().unwrap();
        let key = SigningKey::from_bytes(&[93; 32]);
        let root = SigningKey::from_bytes(&[94; 32]);
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: key.verifying_key().to_bytes(),
            device_label: "Synthetic runtime".into(),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        };
        tilo_user_identity::sign_device_cert(&mut cert, &root).unwrap();
        let identity = DeviceIdentity::kept(cert.clone(), key.clone());
        let endpoint = test_endpoint(&key).await;
        let service = tilo_workspace_service::own_device::open(
            directory.path().join("workspace-host"),
            tilo_workspace_service::own_device::Device {
                cert: &cert,
                signer: &identity.signer,
            },
            endpoint.clone(),
            tilo_workspace_service::own_device::key_groups(
                tilo_workspace_service::apps::workspace_keys::KeyProvider::default(),
                None,
                None,
            ),
            true,
            // The fixture's endpoint names no address anywhere: nothing will,
            // so opening does not wait for one.
            |config| tilo_workspace_service::service::ServiceConfig {
                reaching: tilo_workspace_service::Reaching::OnItsOwn,
                ..config
            },
        )
        .await
        .unwrap();
        let service = Arc::new(service);
        service.workspaces().set_member_device(Arc::new(
            tilo_workspace_service::workspaces::MemberDevice {
                cert: cert.clone(),
                signer: Arc::clone(&identity.signer),
            },
        ));
        let workspaces = Self::new(Arc::clone(&service), identity, endpoint, BTreeSet::new());
        service
            .hold_own_workspace(&workspace.to_string(), "Home", &cert)
            .await
            .unwrap();
        (workspaces, directory)
    }

    /// `app`'s `record` as the workspace service keeps it in `workspace`: its
    /// latest whole write, opened with the channel's keys. `Err` says where
    /// the read stops, so a write that never arrives is explained.
    pub(in crate::standalone) fn test_kept_document(
        &self,
        workspace: WorkspaceId,
        app: &str,
        record: &str,
    ) -> Result<Vec<u8>, String> {
        let copies = self.service.workspaces().clone();
        let only = BTreeSet::from([(app.to_owned(), record.to_owned())]);
        let (app, record) = (app.to_owned(), record.to_owned());
        block_on_owned(async move {
            let whole = copies
                .whole_documents(&workspace.to_string(), Some(&only))
                .await
                .map_err(|error| error.to_string())?;
            if let Some((_, reason)) = whole.unread.first() {
                return Err(reason.clone());
            }
            whole
                .documents
                .iter()
                .find(|((kept_app, kept_record), _)| kept_app == &app && kept_record == &record)
                .map(|(_, bytes)| bytes.clone())
                .ok_or_else(|| "the workspace keeps no such record".to_owned())
        })
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a test fixture failing is its test failing"
)]
async fn test_endpoint(key: &ed25519_dalek::SigningKey) -> iroh::Endpoint {
    iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(iroh::SecretKey::from_bytes(&key.to_bytes()))
        .relay_mode(iroh::RelayMode::Disabled)
        .bind_addr((std::net::Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .bind()
        .await
        .unwrap()
}

/// Run `future` on the app runtime, from a thread that is not one of its
/// tasks: what the synchronous calls into the service use.
///
/// On a thread already driving a runtime a second `block_on` would panic, so
/// the future is driven from a thread of its own there.
pub(super) fn block_on_owned<F>(future: F) -> F::Output
where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
{
    let runtime = tilo_rt::app();
    if tokio::runtime::Handle::try_current().is_err() {
        return runtime.handle().block_on(future);
    }
    std::thread::scope(
        |scope| match scope.spawn(|| runtime.handle().block_on(future)).join() {
            Ok(output) => output,
            Err(panic) => std::panic::resume_unwind(panic),
        },
    )
}
