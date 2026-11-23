//! Every workspace this computer takes part in, as the device's one workspace service holds it.
//!
//! One service holds every workspace: the member copies of the workspaces the login takes part
//! in, synced with the server that hosts them, and, on a computer with no login, the workspace
//! founded here, which the service governs. The daemon keeps no replica of its own: what it reads
//! (a workspace's name and governance, the person the device acts for there, the people holding
//! its keys) it reads from the service, and what it writes it commits there.
//!
//! The device runs one endpoint under its key and one key service: the session the login holds
//! with its server dials from that endpoint and reads and asks with those keys
//! ([`tilo_session_client::Lent`]), and the member copies sync over the same endpoint and settle
//! their groups' keys through the same key service. A second endpoint or key service under the
//! device's key would take the first one's place at the server and in each group.

use std::{
    path::Path,
    sync::{Arc, Mutex, OnceLock},
};

use ed25519_dalek::SigningKey;
use tilo_desktop_model::{Layout, Workspace, WorkspaceId};
use tilo_identity::broker::WorkspaceMembership;
use tilo_identity::identity_store::DeviceIdentity;
use tilo_peer_state::StatePeer;
use tilo_peer_state::actions::{ReadFuture, Workspaces as PeerWorkspaces};
use tilo_peer_state::workspace_membership::WorkspaceMembershipCommand;
use tilo_session_client::device::Device as Entered;
use tilo_workspace_service::apps::services::{GrantRecording, WorkspaceAuthority, WorkspaceGrants};
use tilo_workspace_service::content::RouteContent;
use tilo_workspace_service::own_device::{self, KeyService};
use tilo_workspace_service::service::WorkspaceService;
use tilo_workspace_service::workspaces::{SessionAdmission, Workspaces};

/// Where the service keeps its store, below the device's directory.
const SERVICE: &str = "service";
/// Where the key service keeps its sealed group state, below the device's directory.
const GROUPS: &str = "groups";

/// What the device's workspace service is opened with.
pub(crate) struct Opening<'a> {
    /// The device's directory: the service's store and the key service's state are below it.
    pub(crate) dir: &'a Path,
    pub(crate) key: &'a SigningKey,
    pub(crate) identity: DeviceIdentity,
    /// The endpoint, bound under the device's key, every link of the device runs on.
    pub(crate) endpoint: iroh::Endpoint,
    /// Whether the service founds and governs workspaces here: on a computer with no login.
    pub(crate) governs: bool,
    /// What the groups' members seal is checked against: the apps' channels.
    pub(crate) seal_checks: Arc<dyn tilo_workspace_keys::SealCheckRegistry>,
    /// What answers the person's keys' record actions in a workspace governed here.
    pub(crate) apps: Option<Arc<dyn tilo_workspace_service::app_serving::AppActionServer>>,
}

/// Every workspace this computer takes part in, through the workspace service this device runs.
pub(crate) struct ServiceWorkspaces {
    service: Arc<WorkspaceService>,
    identity: DeviceIdentity,
    endpoint: iroh::Endpoint,
    /// The endpoint as the login's session is lent it.
    bound: Arc<OnceLock<iroh::Endpoint>>,
    /// The device's key service, which settles the keys of every group the device takes part in.
    keys: Option<KeyService>,
    /// The workspace founded here, which the service governs, once it is.
    own: OnceLock<WorkspaceId>,
    /// The session this device entered with, once a holder ran: what the device's own actions are
    /// asked of its host through.
    session: std::sync::OnceLock<Entered>,
    /// Whether this device takes its server's work, and so is one of its person's servers.
    takes_work: std::sync::atomic::AtomicBool,
    /// The record content service of these workspaces, once it is made.
    content: OnceLock<Arc<RouteContent>>,
    /// What the service hands the content its members answer this device with,
    /// held here so it is there for as long as the runtime.
    keepers: Mutex<Vec<Arc<dyn tilo_workspace_service::workspaces::ContentKeeper>>>,
}

impl ServiceWorkspaces {
    /// Open the device's workspace service and its key service below `opening.dir`.
    ///
    /// # Errors
    /// The key service's or the service's store cannot be opened.
    pub(crate) async fn open(opening: Opening<'_>) -> Result<Arc<Self>, String> {
        let Opening {
            dir,
            key,
            identity,
            endpoint,
            governs,
            seal_checks,
            apps,
        } = opening;
        let keys = KeyService::open(
            "tilo-device-groups",
            &dir.join(GROUPS),
            vault_key(key),
            Arc::clone(&identity.signer),
            seal_checks,
        )
        .map_err(|error| format!("this device's key service: {error}"))?;
        let service = own_device::open(
            dir.join(SERVICE),
            own_device::Device {
                cert: &identity.device_cert,
                signer: &identity.signer,
            },
            endpoint.clone(),
            keys.groups(),
            governs,
            |config| tilo_workspace_service::service::ServiceConfig { apps, ..config },
        )
        .await
        .map_err(|error| format!("this device's workspace service: {error}"))?;
        keys.attach(service.workspaces());
        let mut opened = Self::unopened(Arc::new(service), identity, endpoint);
        opened.keys = Some(keys);
        Ok(Arc::new(opened))
    }

    /// A service already opened, holding member copies alone, with no key service of its own.
    pub(crate) fn new(
        service: Arc<WorkspaceService>,
        identity: DeviceIdentity,
        endpoint: iroh::Endpoint,
    ) -> Arc<Self> {
        Arc::new(Self::unopened(service, identity, endpoint))
    }

    fn unopened(
        service: Arc<WorkspaceService>,
        identity: DeviceIdentity,
        endpoint: iroh::Endpoint,
    ) -> Self {
        let bound = Arc::new(OnceLock::new());
        let _ = bound.set(endpoint.clone());
        Self {
            service,
            identity,
            endpoint,
            bound,
            keys: None,
            own: OnceLock::new(),
            session: std::sync::OnceLock::new(),
            takes_work: std::sync::atomic::AtomicBool::new(false),
            content: OnceLock::new(),
            keepers: Mutex::new(Vec::new()),
        }
    }

    /// Found `workspace`, called `name`, here unless the service holds it already, and enter it
    /// as this device: the workspace of a computer with no login, whose member list names the
    /// person as its owner.
    ///
    /// # Errors
    /// The service refused to found it, or to let this device in.
    pub(crate) async fn found(&self, workspace: WorkspaceId, name: &str) -> Result<(), String> {
        let id = workspace.to_string();
        let cert = &self.identity.device_cert;
        self.service.hold_own_workspace(&id, name, cert).await?;
        self.service
            .workspaces()
            .enter(&id, cert)
            .await
            .map(drop)
            .map_err(|error| error.to_string())?;
        let _ = self.own.set(workspace);
        Ok(())
    }

    /// The device's key service.
    pub(crate) fn keys(&self) -> tilo_workspace_service::apps::workspace_keys::KeyProvider {
        self.keys.as_ref().map(KeyService::keys).unwrap_or_default()
    }

    /// What the login's session runs on: this device's endpoint and key service.
    pub(crate) fn lent(&self) -> tilo_session_client::Lent {
        tilo_session_client::Lent {
            keys: self.keys().get(),
            endpoint: Some(Arc::clone(&self.bound)),
            own: true,
        }
    }

    /// Hold each workspace `device` entered as a member copy here, synced with the server that
    /// hosts it: those not held yet, each at the address its host signed as it let this device in.
    /// This service is the device's one copy of them; the session keeps none of its own.
    ///
    /// A device that takes its server's work enters itself among its person's servers through
    /// its first session, at the address its endpoint answers on, as a server does on each start:
    /// the server finds it there for its workspaces' Linux turns.
    pub(crate) async fn hold(&self, device: &Entered) {
        if self.session.set(device.clone()).is_ok()
            && self.takes_work.load(std::sync::atomic::Ordering::Relaxed)
        {
            let me = (Some(device), &self.identity.device_cert);
            let at = Some(self.endpoint.addr());
            if let Err(error) = super::ticket::server_entry(self.copies(), me, at, false).await {
                tracing::warn!(%error, "this device was not entered among its person's servers");
            }
        }
        own_device::hold_as_members(self.service.workspaces(), device.hosts()).await;
    }

    /// Close the service and its endpoint, as the process ending would: what the service holds
    /// is kept, and the next service opened under this device's directory takes it up.
    #[cfg(test)]
    pub(crate) async fn shutdown(&self) {
        self.service.shutdown().await;
        self.endpoint.close().await;
    }

    /// This device takes its server's work: its first session enters it among its person's servers.
    pub(crate) fn take_work(&self) {
        self.takes_work
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    /// The session this device entered with, once a holder ran: its own actions are asked of its
    /// host through it.
    pub(crate) fn session(&self) -> Option<&Entered> {
        self.session.get()
    }

    /// The workspace service this device runs.
    pub(crate) fn service(&self) -> &Arc<WorkspaceService> {
        &self.service
    }

    /// The service's workspaces, as this device holds them.
    pub(crate) fn copies(&self) -> &Workspaces {
        self.service.workspaces()
    }

    /// This device's endpoint, which the profile's other services share.
    pub(crate) fn endpoint(&self) -> iroh::Endpoint {
        self.endpoint.clone()
    }

    /// What signs as this device, as its action service signs every request it
    /// makes.
    pub(crate) fn signer(&self) -> Arc<dyn collab_types::signer::DeviceSigner> {
        Arc::clone(&self.identity.signer)
    }

    /// The device certificate this profile's ownership holds.
    pub(crate) fn device_cert(&self) -> &tilo_user_identity::DeviceCert {
        &self.identity.device_cert
    }

    /// The person this device belongs to: whose devices may ask it for work. Only a host that
    /// takes work asks, which Linux is the one of today.
    #[cfg(target_os = "linux")]
    pub(crate) fn operator(&self) -> [u8; 32] {
        self.identity.device_cert.user_public_key
    }

    /// Told each time this profile starts holding a workspace as a member.
    pub(crate) fn entered(&self) -> Arc<tokio::sync::Notify> {
        self.copies().members_held()
    }

    /// Whether this profile holds `workspace` as a member copy.
    pub(crate) fn holds(&self, workspace: WorkspaceId) -> bool {
        let copies = self.copies().clone();
        block_on_owned(async move { copies.member_ids().await.contains(&workspace) })
    }

    /// Whether this device keeps `workspace`: its member copy, or the workspace founded here.
    pub(crate) async fn keeps(&self, workspace: WorkspaceId) -> bool {
        copy_of(self.copies(), workspace).await.is_some()
    }

    /// What reads `workspace`'s policy as this device's copy of it holds it now, `None` while the
    /// copy is not here or its policy does not read.
    pub(crate) fn policy_of(
        &self,
        workspace: WorkspaceId,
    ) -> impl Fn() -> Option<tilo_action_policy::WorkspacePolicy> + Send + Sync + 'static {
        let copies = self.copies().clone();
        move || {
            let copies = copies.clone();
            block_on_owned(async move { copy_of(&copies, workspace).await })
                .and_then(|(_, peer)| peer.governance().governed_policy().ok())
        }
    }

    /// `workspace`'s replica as this device holds it: its member copy, or the
    /// workspace the service governs here.
    pub(crate) fn peer(&self, workspace: WorkspaceId) -> Option<StatePeer> {
        let copies = self.copies().clone();
        block_on_owned(async move { copy_of(&copies, workspace).await.map(|(_, peer)| peer) })
    }

    /// The workspaces this device holds, each with the name its governance gives it and the
    /// replica it keeps: the member copies, and the workspace founded here.
    pub(crate) fn held(&self) -> Vec<(WorkspaceId, String, StatePeer)> {
        let workspaces = self.copies().clone();
        let own = self.own.get().copied();
        block_on_owned(async move {
            let mut held = Vec::new();
            if let Some(own) = own
                && let Some((name, peer)) = copy_of(&workspaces, own).await
            {
                held.push((own, name, peer));
            }
            for workspace in workspaces.member_ids().await {
                if let Some((name, peer)) = workspaces.member_copy(workspace).await {
                    held.push((workspace, name, peer));
                }
            }
            held
        })
    }

    /// Every workspace this device holds, as its governance describes it.
    pub(crate) fn workspaces(&self) -> Vec<Workspace> {
        self.held()
            .into_iter()
            .filter_map(|(workspace, name, peer)| {
                tilo_workspace_service::workspaces::describe(workspace, &name, &peer)
            })
            .collect()
    }

    /// One workspace as [`Self::workspaces`] describes it, built alone.
    pub(crate) fn workspace(&self, workspace: WorkspaceId) -> Option<Workspace> {
        let copies = self.copies().clone();
        block_on_owned(async move {
            let (name, peer) = copy_of(&copies, workspace).await?;
            tilo_workspace_service::workspaces::describe(workspace, &name, &peer)
        })
    }

    /// What this device is to `workspace`, for the workspace's Files endpoint:
    /// the certificate the workspace holds for this device's key.
    ///
    /// # Errors
    /// `workspace` is no workspace id, or the certificate is not valid now.
    pub(crate) fn files_credentials(
        &self,
        workspace: &str,
    ) -> std::io::Result<tilo_fs::identity::PeerCredentials> {
        let workspace = WorkspaceId::parse(workspace)
            .ok_or_else(|| std::io::Error::other("not a workspace id"))?;
        if !self.holds(workspace) && self.own.get() != Some(&workspace) {
            return Err(std::io::Error::other("the workspace is not held here"));
        }
        let (cert, _) = self.identity.material_for(workspace);
        tilo_fs::identity::PeerCredentials::certified(
            cert,
            self.signer(),
            tilo_clock::unix_now_ns(),
        )
        .map_err(std::io::Error::other)
    }

    /// Select only a workspace held here that this device's actor currently takes part in, as
    /// that person.
    ///
    /// # Errors
    /// The workspace is not held here, or `actor` is not the person this device acts for in it.
    pub(crate) fn action_workspace(&self, actor: &str, workspace: &str) -> Result<String, String> {
        let workspace = WorkspaceId::parse(workspace).ok_or("invalid MCP workspace")?;
        let peer = self.peer(workspace).ok_or("no such workspace held")?;
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
    pub(crate) async fn claim_lease(
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
    pub(crate) async fn admit_execution(
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
    pub(crate) fn recheck_executions(&self, workspace: WorkspaceId) -> Result<(), String> {
        self.copies().recheck_executions(workspace)
    }

    /// What asks the workspaces' other devices, through this device's own
    /// workspace host, to take an offloadable action's turn: the action
    /// service, its endpoint and runtime.
    pub(crate) fn turn_action_service(
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
    pub(crate) fn follow_admitted_writes(
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
    pub(crate) fn record_content_service(&self) -> Option<Arc<RouteContent>> {
        self.content.get().cloned()
    }

    /// Keep `content` as this profile's record content service from now on.
    pub(crate) fn install_content(&self, content: &Arc<RouteContent>) {
        let _ = self.content.set(Arc::clone(content));
    }

    /// Keep `keeper`, which the service hands the content its members answer
    /// this device with.
    pub(crate) fn install_keeper(
        &self,
        keeper: Arc<dyn tilo_workspace_service::workspaces::ContentKeeper>,
    ) {
        if let Ok(mut keepers) = self.keepers.lock() {
            keepers.push(keeper);
        }
    }
}

impl WorkspaceAuthority for ServiceWorkspaces {
    /// The revocations the workspace's replica on this device holds: what an agent's key and a
    /// saved shortcut's signer are checked against here.
    fn workspace_revocations(&self, workspace: &str) -> Option<tilo_user_identity::RevocationList> {
        let peer = self.peer(WorkspaceId::parse(workspace)?)?;
        let mut revocations = tilo_user_identity::RevocationList::new();
        for entry in peer.revocations() {
            revocations.insert(entry).ok()?;
        }
        Some(revocations)
    }

    /// Every workspace this device holds, as one tree.
    fn with_tree(&self, read: &mut dyn FnMut(&Layout)) -> Result<(), String> {
        let tree = Layout {
            workspaces: self.workspaces(),
            ..Layout::default()
        };
        read(&tree);
        Ok(())
    }

    /// Only the requested workspace is built.
    fn with_workspace(
        &self,
        workspace: &str,
        read: &mut dyn FnMut(Option<&Workspace>),
    ) -> Result<(), String> {
        let held = WorkspaceId::parse(workspace)
            .and_then(|id| self.workspace(id))
            .filter(|held| held.id().to_string() == workspace);
        read(held.as_ref());
        Ok(())
    }
}

impl WorkspaceMembership for ServiceWorkspaces {
    fn with_workspace(&self, workspace: &WorkspaceId, read: &mut dyn FnMut(&Workspace)) {
        if let Some(held) = self.workspace(*workspace) {
            read(&held);
        }
    }
}

/// The grants people give apps for other apps' actions: each a change to the workspace's member
/// list, committed through the service as every other member's change is, and in force once the
/// service took it in.
pub(crate) struct MemberListGrants {
    pub(crate) workspaces: Arc<ServiceWorkspaces>,
    /// The person this device acts for: the one who may grant here.
    pub(crate) person: String,
    /// The apps a grant names, by the names its receipt shows them with.
    pub(crate) apps: Arc<crate::apps::Apps>,
}

impl MemberListGrants {
    /// Commit `change` to `workspace`'s member list: the member copy's, sent to the device
    /// serving it, or the list of the workspace governed here, with `receipt`, what the
    /// workspace's history says of it, in the same change.
    async fn commit(
        workspaces: Arc<ServiceWorkspaces>,
        workspace: String,
        change: tilo_action_policy::Change,
        receipt: Option<tilo_desktop_model::WorkspaceEffectReceipt>,
    ) -> Result<(), String> {
        let command = WorkspaceMembershipCommand::Member { change };
        let copies = workspaces.copies();
        if copies.is_member(&workspace).await.unwrap_or(false) {
            let commit = copies.member_membership_write(&workspace, command).await?;
            copies.commit_member_document(&workspace, commit).await?;
        } else {
            copies
                .commit_own_membership(&workspace, command, receipt.as_ref())
                .await
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }
}

impl WorkspaceGrants for MemberListGrants {
    fn may_record(&self, workspace: &str, person: &str) -> bool {
        person == self.person
            && WorkspaceId::parse(workspace)
                .and_then(|id| self.workspaces.workspace(id))
                .is_some_and(|held| {
                    held.may_govern_at(
                        person,
                        tilo_action_policy::roles::governance::GRANT,
                        tilo_clock::unix_now(),
                    )
                })
    }

    fn record(
        &self,
        request: &tilo_workspace_service::apps::app_actions::AppActionConsentRequest,
        grant: tilo_action_policy::Grant,
    ) -> GrantRecording {
        let id = grant.id.as_str().to_owned();
        // The person's history says who allowed which app what, as every change of theirs.
        let receipt = tilo_desktop_model::WorkspaceEffectReceipt::recorded(
            tilo_actions::InvocationId::new(),
            &tilo_actions::MemberId::person(request.person.clone()),
            tilo_desktop_model::grants::app_action_grant_context(
                &request.workspace,
                &self.apps.display_name(&request.app_id),
                &self.apps.display_name(&request.target_app),
                &request.action_label,
                grant.lifetime.key(),
            ),
            tilo_actions::EffectReceiptStatus::Succeeded,
            i64::try_from(tilo_clock::unix_now()).unwrap_or(i64::MAX),
        );
        let committed = Self::commit(
            Arc::clone(&self.workspaces),
            request.workspace.clone(),
            tilo_action_policy::Change::Grant(grant),
            Some(receipt),
        );
        Box::pin(async move {
            committed
                .await
                .map_err(|error| format!("this workspace could not record the grant: {error}"))?;
            Ok(id)
        })
    }

    fn revoke(&self, workspace: &str, grant: &str) {
        let committed = Self::commit(
            Arc::clone(&self.workspaces),
            workspace.to_owned(),
            tilo_action_policy::Change::Revoke(tilo_action_policy::GrantId::new(grant)),
            None,
        );
        let grant = grant.to_owned();
        tilo_rt::app().spawn(async move {
            if let Err(error) = committed.await {
                tracing::warn!(grant, %error, "a grant given once could not be taken back");
            }
        });
    }
}

/// `workspace` as `workspaces` hold it here, with its name: its member copy, or the workspace
/// the service founded and keeps here.
async fn copy_of(
    workspaces: &tilo_workspace_service::workspaces::Workspaces,
    workspace: WorkspaceId,
) -> Option<(String, StatePeer)> {
    match workspaces.member_copy(workspace).await {
        Some(copy) => Some(copy),
        None => workspaces.governed_copy(workspace).await,
    }
}

/// The key sealing the device's group state: derived from the device key, which this computer
/// keeps beside it.
fn vault_key(key: &SigningKey) -> key_group::VaultKey {
    key_group::VaultKey::new(blake3::derive_key(
        "tilo device workspace key vault v1",
        &key.to_bytes(),
    ))
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
pub(crate) struct Reads {
    pub(crate) service: Arc<WorkspaceService>,
    pub(crate) entering: Option<Arc<super::take_work::Entering>>,
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
        placement: Option<&'a tilo_peer_state::join::TransportHint>,
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
            if let Err(reason) = entering.enter(workspace, placement).await {
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
        let Some((_, peer)) = copy_of(workspaces, id).await else {
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
    pub(crate) async fn test_hosting(workspace: WorkspaceId) -> (Arc<Self>, tempfile::TempDir) {
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
        let workspaces = Self::new(Arc::clone(&service), identity, endpoint);
        workspaces.found(workspace, "Home").await.unwrap();
        (workspaces, directory)
    }

    /// A service governing `workspace` alone, on a profile of its own, as a machine whose person
    /// this account holds: a real identity store, its own key service, and no login. What the
    /// stack's own person is, and what a test that founds the person's Servers workspace needs.
    /// Answers the device's key and certificate, and where its identity is kept.
    pub(crate) async fn test_hosting_person(
        workspace: WorkspaceId,
    ) -> (
        Arc<Self>,
        tempfile::TempDir,
        ed25519_dalek::SigningKey,
        tilo_user_identity::DeviceCert,
        tilo_device_identity::IdentityPaths,
    ) {
        let directory = tempfile::tempdir().unwrap();
        let paths = tilo_device_identity::IdentityPaths::under(directory.path().join("keys"));
        tilo_identity::provisioning::create_person(
            directory.path(),
            &paths,
            "",
            "Synthetic runtime",
            tilo_identity::provisioning::IdentityStorage::Files,
        )
        .unwrap();
        let key = tilo_device_identity::persist::read_device_key(&paths.device_privkey).unwrap();
        let cert = tilo_device_identity::persist::read_device_cert(&paths.device_cert).unwrap();
        let identity = DeviceIdentity::held(
            cert.clone(),
            key.clone(),
            Arc::new(tilo_identity::identity_store::files::Files::new(
                paths.clone(),
            )),
        );
        let endpoint = test_endpoint(&key).await;
        let sessions = crate::key_sessions::KeySessions::new();
        let workspaces = Self::open(Opening {
            dir: &directory.path().join("profile"),
            key: &key,
            identity,
            endpoint,
            governs: true,
            seal_checks: Arc::new(tilo_workspace_keys::NoSealChecks),
            apps: Some(Arc::clone(&sessions)
                as Arc<dyn tilo_workspace_service::app_serving::AppActionServer>),
        })
        .await
        .unwrap();
        workspaces.found(workspace, "Home").await.unwrap();
        (workspaces, directory, key, cert, paths)
    }

    /// The service a logged-in computer keeps below `dir` as the device `key` certified by
    /// `cert`, on an endpoint bound the way its session reaches `server`.
    pub(crate) async fn open_for_test(
        dir: &Path,
        key: &SigningKey,
        cert: &tilo_user_identity::DeviceCert,
        server: &tilo_server_client::ServerAddress,
    ) -> Result<Arc<Self>, String> {
        let endpoint = server
            .bind(Some(key.to_bytes()))
            .await
            .map_err(|error| format!("this device's endpoint: {error}"))?;
        Self::open(Opening {
            dir,
            key,
            identity: DeviceIdentity::kept(cert.clone(), key.clone()),
            endpoint,
            governs: false,
            seal_checks: Arc::new(tilo_workspace_keys::NoSealChecks),
            apps: None,
        })
        .await
    }

    /// `app`'s `record` as the workspace service keeps it in `workspace`: its
    /// latest whole write, opened with the channel's keys. `Err` says where
    /// the read stops, so a write that never arrives is explained.
    pub(crate) fn test_kept_document(
        &self,
        workspace: WorkspaceId,
        app: &str,
        record: &str,
    ) -> Result<Vec<u8>, String> {
        let copies = self.service.workspaces().clone();
        let only = std::collections::BTreeSet::from([(app.to_owned(), record.to_owned())]);
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
pub(crate) fn block_on_owned<F>(future: F) -> F::Output
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

#[cfg(test)]
#[path = "workspaces_tests.rs"]
mod tests;
