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
use tilo_actions::remote::invocation_record::Invocations;
use tilo_desktop_model::{Layout, Workspace, WorkspaceId};
use tilo_identity::broker::WorkspaceMembership;
use tilo_identity::identity_store::DeviceIdentity;
use tilo_peer_state::StatePeer;
use tilo_peer_state::actions::{ReadFuture, Workspaces as PeerWorkspaces};
use tilo_peer_state::workspace_membership::WorkspaceMembershipCommand;
use tilo_session_client::device::Device as Entered;
use tilo_workspace_service::apps::services::{GrantRecording, WorkspaceAuthority, WorkspaceGrants};
use tilo_workspace_service::content::{ContentShelf, RouteContent};
use tilo_workspace_service::own_device::{self, KeyService};
use tilo_workspace_service::service::{ServiceConfig, WorkspaceService};
use tilo_workspace_service::workspaces::{SessionAdmission, Workspaces};

/// Where the key service keeps its sealed group state, below the device's directory.
const GROUPS: &str = "groups";
/// The file of the groups' directory naming which group reads each workspace's documents.
const READER_GROUPS: &str = "reader-groups.json";

/// What a desktop of this device kept itself before this service kept its person's workspaces,
/// as it hands it over once ([`ServiceWorkspaces::import`]).
#[derive(Debug, Default, serde::Deserialize)]
pub(crate) struct Carried {
    /// The key its key groups were sealed with, in hex.
    #[serde(default)]
    pub(crate) sealed_with: Option<String>,
    /// Each file of its key groups' directory by name, its bytes in hex.
    #[serde(default)]
    pub(crate) groups: std::collections::BTreeMap<String, String>,
    /// The workspaces it took part in.
    #[serde(default)]
    pub(crate) workspaces: Vec<CarriedWorkspace>,
    /// Whether it was to found its person's own workspace and had not yet
    /// ([`tilo_identity::provisioning::founds_person_workspace`]).
    #[serde(default)]
    pub(crate) founds_person_workspace: bool,
    /// What its own workspace service kept, as [`tilo_workspace_service::workspaces::MovingService::to_hex`]
    /// hands it over.
    #[serde(default)]
    pub(crate) service: Option<String>,
}

/// A workspace a desktop took part in.
#[derive(Debug, serde::Deserialize)]
pub(crate) struct CarriedWorkspace {
    pub(crate) workspace: String,
    /// The address its host signed for it, where this device takes part in it as a member.
    #[serde(default)]
    pub(crate) hint: Option<tilo_peer_state::join::TransportHint>,
}

/// What the device's workspace service is opened with.
pub(crate) struct Opening<'a> {
    /// The device's directory: the service's store and the key service's state are below it.
    pub(crate) dir: &'a Path,
    pub(crate) key: &'a SigningKey,
    pub(crate) identity: DeviceIdentity,
    /// The endpoint, bound under the device's key, every link of the device runs on.
    pub(crate) endpoint: iroh::Endpoint,
    /// What the groups' members seal is checked against: the apps' channels.
    pub(crate) seal_checks: Arc<dyn tilo_workspace_keys::SealCheckRegistry>,
    /// What answers the person's keys' record actions in a workspace governed here.
    pub(crate) apps: Option<Arc<dyn tilo_workspace_service::app_serving::AppActionServer>>,
    /// What the service keeps beyond a device's own, on a server.
    pub(crate) configure: Option<Box<dyn FnOnce(ServiceConfig) -> ServiceConfig + Send + 'a>>,
    /// The device itself, when the service acts as it: on a server.
    pub(crate) itself: Option<tilo_workspace_service::service::ServiceIdentity>,
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
    /// Where that key service keeps its groups' state, and the vault it seals them under: what a
    /// desktop's groups are taken into ([`Self::import`]).
    groups: Option<(std::path::PathBuf, key_group::VaultKey)>,
    /// The session this device entered with, once a holder ran: what the device's own actions are
    /// asked of its host through.
    session: std::sync::OnceLock<Entered>,
    /// Whether this device takes its server's work, and so is one of its person's servers.
    takes_work: std::sync::atomic::AtomicBool,
    /// The record content service of these workspaces, once it is made, and the shelf it keeps
    /// content on.
    content: OnceLock<(Arc<RouteContent>, Arc<dyn ContentShelf>)>,
    /// What the service hands the content its members answer this device with,
    /// held here so it is there for as long as the runtime.
    keepers: Mutex<Vec<Arc<dyn tilo_workspace_service::workspaces::ContentKeeper>>>,
    /// The device's one search index, once the daemon keeps it: what every caller's search of
    /// these workspaces is answered from.
    search: OnceLock<Arc<super::search::DeviceSearch>>,
    /// The records the device keeps as its own, once the person's services run: its settings,
    /// read through the one record API in its own space and in the person's workspace.
    records: OnceLock<Arc<super::records::DeviceRecords>>,
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
            seal_checks,
            apps,
            configure,
            itself,
        } = opening;
        let own_device::Opened { service, keys } = own_device::open_in(
            dir,
            own_device::Device {
                cert: &identity.device_cert,
                signer: &identity.signer,
                itself,
            },
            own_device::vault_key(key),
            endpoint.clone(),
            seal_checks,
            |config| {
                let config = ServiceConfig { apps, ..config };
                match configure {
                    Some(configure) => configure(config),
                    // A person's computer serves its workspaces at once, on the network or off
                    // it: the service names its address in them once a relay answers, so
                    // starting never waits out a net report. One with no relays names none.
                    None => ServiceConfig {
                        reaching: if tilo_endpoint_reachability::relays().relays.is_empty() {
                            tilo_workspace_service::Reaching::OnItsOwn
                        } else {
                            tilo_workspace_service::Reaching::WhenNeeded
                        },
                        ..config
                    },
                }
            },
        )
        .await
        .map_err(|error| format!("this device's workspace service: {error}"))?;
        let mut opened = Self::unopened(Arc::new(service), identity, endpoint);
        opened.keys = Some(keys);
        opened.groups = Some((dir.join(GROUPS), own_device::vault_key(key)));
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
            groups: None,
            session: std::sync::OnceLock::new(),
            takes_work: std::sync::atomic::AtomicBool::new(false),
            content: OnceLock::new(),
            keepers: Mutex::new(Vec::new()),
            search: OnceLock::new(),
            records: OnceLock::new(),
        }
    }

    /// Keep the device's records `records`, once the person's services run: what the settings
    /// actions write and what reads in the person's workspace answer.
    pub(crate) fn hold_device_records(&self, records: Arc<super::records::DeviceRecords>) {
        let _ = self.records.set(records);
    }

    /// The records this device keeps as its own, once the person's services kept them.
    pub(crate) fn device_records(&self) -> Option<&Arc<super::records::DeviceRecords>> {
        self.records.get()
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
        Ok(())
    }

    /// Take in, once, what a desktop of this device kept itself before this service kept its
    /// person's workspaces (`account/workspaces#import`): each key group the desktop sealed, opened
    /// with the key it was sealed with and sealed again under this device's vault beside the groups
    /// the key service keeps, which opens it once the service takes part in its workspace; and each
    /// workspace the desktop took part in, entered here as a member at the address its host
    /// signed. A group the key service keeps already stays as it is: the device keeps the one leaf
    /// it has in each group, and no group is founded again beside one the desktop had. The
    /// workspaces the desktop's own service governed move here, after the groups
    /// ([`tilo_workspace_service::workspaces::Workspaces::move_in`]), and are answered beside
    /// the answer. When the desktop was to found its person's own workspace and had not yet, this
    /// service founds it, after the groups are in.
    ///
    /// # Errors
    /// The device keeps no key service, or a group cannot be read, opened or written.
    pub(crate) async fn import(
        &self,
        carried: Carried,
    ) -> Result<(serde_json::Value, Vec<WorkspaceId>), String> {
        let groups = match &carried.sealed_with {
            Some(sealed_with) => self.take_in_groups(sealed_with, &carried.groups)?,
            None => Vec::new(),
        };
        // The key service reads the groups, their readers and the routes of document groups as
        // it opens: those just written are read now, before the workspaces sealed with them move.
        if let Some(keys) = self.keys().get() {
            keys.take_in_kept();
        }
        let moved = match &carried.service {
            Some(service) => {
                let service = tilo_workspace_service::workspaces::MovingService::from_hex(service)?;
                self.copies().move_in(service).await?
            }
            None => tilo_workspace_service::workspaces::MovedIn::default(),
        };
        let person = &self.identity.device_cert.user_public_key;
        let named = tilo_server_protocol::servers::person_workspace_id(person);
        let person = WorkspaceId::parse(&named)
            .ok_or_else(|| format!("a person's workspace id {named} is no workspace id"))?;
        let founded = if carried.founds_person_workspace && !moved.moved.contains(&person) {
            let workspace = person;
            let name = tilo_workspace_service::workspaces::person_workspace::NAME;
            self.found(workspace, name)
                .await
                .inspect_err(|error| {
                    tracing::warn!(%error, "this device did not found its person's own workspace");
                })
                .is_ok()
        } else {
            false
        };
        let entering: Vec<_> = carried
            .workspaces
            .iter()
            .filter_map(|workspace| {
                Some((
                    WorkspaceId::parse(&workspace.workspace)?,
                    workspace.hint.clone()?,
                ))
            })
            .collect();
        let entered: Vec<String> = entering.iter().map(|(id, _)| id.to_string()).collect();
        let copies = self.copies().clone();
        tokio::spawn(async move { copies.hold_as_members(entering).await });
        let answer = serde_json::json!({
            "groups": groups.iter().map(ToString::to_string).collect::<Vec<_>>(),
            "entering": entered,
            "founded_person_workspace": founded,
            "moved": moved.moved.iter().map(ToString::to_string).collect::<Vec<_>>(),
            "refused": moved
                .refused
                .iter()
                .map(|(workspace, error)| serde_json::json!({"workspace": workspace.to_string(), "error": error}))
                .collect::<Vec<_>>(),
        });
        Ok((answer, moved.moved))
    }

    /// Seal `groups`, the files of a desktop's key groups sealed under `sealed_with`, again under
    /// this device's vault beside the key service's own: the workspaces whose groups were taken.
    fn take_in_groups(
        &self,
        sealed_with: &str,
        groups: &std::collections::BTreeMap<String, String>,
    ) -> Result<Vec<WorkspaceId>, String> {
        let (dir, vault) = self
            .groups
            .as_ref()
            .ok_or("this device keeps no key groups to take a desktop's into")?;
        let from: [u8; 32] = hex::decode(sealed_with)
            .ok()
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or("the key the desktop's groups were sealed with is not 32 bytes in hex")?;
        let from = key_group::VaultKey::new(from);
        tilo_files::native::atomic::create_private_directory(dir)
            .map_err(|error| format!("{}: {error}", dir.display()))?;
        let mut taken = Vec::new();
        for (name, bytes) in groups {
            // A plain file name only: what a desktop hands names nothing outside the groups.
            if Path::new(name)
                .file_name()
                .and_then(std::ffi::OsStr::to_str)
                != Some(name.as_str())
                || name.starts_with('.')
            {
                return Err(format!("a desktop's key group file is named {name:?}"));
            }
            let bytes =
                hex::decode(bytes).map_err(|_| format!("the desktop's {name} is not hex"))?;
            let path = dir.join(name);
            let written = match name.strip_suffix(".peer") {
                Some(_) if path.exists() => continue,
                Some(workspace) => {
                    let workspace = WorkspaceId::parse(workspace)
                        .ok_or_else(|| format!("the desktop's {name} names no workspace"))?;
                    let signer: Box<dyn key_group::DeviceSigner> =
                        Box::new(Arc::clone(&self.identity.signer));
                    let peer = match key_group::GroupPeer::restore(workspace, signer, &bytes, &from)
                    {
                        Ok(peer) => peer,
                        // A machine copied from another keeps state its source made, which no
                        // device but that one acts in: this device joins as itself.
                        Err(key_group::Error::OtherDevice) => continue,
                        Err(error) => {
                            return Err(format!("the desktop's group of {workspace}: {error}"));
                        }
                    };
                    let sealed = peer
                        .export_sealed(vault)
                        .map_err(|error| format!("the group of {workspace}: {error}"))?;
                    taken.push(workspace);
                    sealed
                }
                None if name == READER_GROUPS => readers_with(&path, &bytes)?,
                None if path.exists() => continue,
                None => bytes,
            };
            tilo_files::native::atomic::write_private(&path, &written)
                .map_err(|error| format!("{}: {error}", path.display()))?;
        }
        Ok(taken)
    }

    /// The device's key service.
    pub(crate) fn keys(&self) -> tilo_workspace_service::apps::workspace_keys::KeyProvider {
        self.keys.as_ref().map(KeyService::keys).unwrap_or_default()
    }

    /// What the login's session runs on: this device's endpoint and key service, and the member
    /// copies it reads and writes through.
    pub(crate) fn lent(&self) -> tilo_session_client::Lent {
        tilo_session_client::Lent {
            keys: self.keys().get(),
            links: Arc::new(MemberLinks(Arc::clone(&self.service))),
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
        self.service
            .workspaces()
            .hold_as_members(device.hosts())
            .await;
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

    /// Share the device's record of the invocations it was asked for with the workspace
    /// service, so what a model call costs is written into the record of the invocation that
    /// made it ([`Invocations::note_spend`]).
    pub(crate) fn serve_invocations(&self, records: Arc<Invocations>) {
        self.service.workspaces().serve_invocations(records);
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
    ///
    /// These readers are called synchronously, from the app runtime's one thread too (a search
    /// asks the action catalogue, which asks which workspaces are held): they read what the
    /// service holds now and never wait on that runtime, which would then wait on itself.
    pub(crate) fn holds(&self, workspace: WorkspaceId) -> bool {
        self.copies()
            .copy_now(workspace)
            .is_some_and(|copy| copy.member)
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
            copies
                .copy_now(workspace)
                .and_then(|copy| copy.peer.governance().governed_policy().ok())
        }
    }

    /// `workspace`'s replica as this device holds it: its member copy, or the
    /// workspace the service governs here.
    pub(crate) fn peer(&self, workspace: WorkspaceId) -> Option<StatePeer> {
        self.copies().copy_now(workspace).map(|copy| copy.peer)
    }

    /// The workspaces this device holds, each with the name its governance gives it and the
    /// copy it keeps: the workspaces served from here first, then the ones it takes part in.
    pub(crate) fn held(&self) -> Vec<(WorkspaceId, String, StatePeer)> {
        // Read without waiting on any workspace's lock or the runtime that serves them: a
        // workspace busy taking in a flood of changes is still listed.
        let mut copies = self.copies().copies_now();
        copies.sort_by_key(|(_, copy)| copy.member);
        copies
            .into_iter()
            .map(|(workspace, copy)| (workspace, copy.name, copy.peer))
            .collect()
    }

    /// Offer the workspaces this device's person makes here as their own, labelled and listed by
    /// this host, and built on by the account's keys.
    pub(crate) fn offer_workspaces_here(&self, host: &str) {
        if let Err(error) = self.copies().offer_own_workspaces(
            tilo_workspace_service::workspaces::OwnWorkspaceOffer::free(host),
        ) {
            tracing::warn!(%error, "this device keeps no workspaces of its person's own");
        }
    }

    /// Record every workspace this device keeps as its person's own, so a workspace founded here
    /// before the host kept an index of their workspaces is theirs to branch and delete.
    pub(crate) async fn claim_workspaces_here(&self, person: &[u8; 32]) {
        for summary in self.copies().list().await {
            if !self.copies().claim_own(person, &summary.workspace).await {
                tracing::warn!(workspace = %summary.workspace, "a workspace this device keeps is not recorded as its person's own");
            }
        }
    }

    /// What `workspace`'s owner labelled it with, as this device keeps it.
    pub(crate) fn labels_of(
        &self,
        workspace: &str,
    ) -> tilo_server_protocol::workspaces::WorkspaceLabels {
        self.copies().labels_of(workspace)
    }

    /// Whether `workspace`'s owner let keys reaching it alone run commands in its machine
    /// without asking them each time.
    pub(crate) fn runs_unattended(&self, workspace: &str) -> bool {
        self.copies().runs_unattended(workspace)
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
        let copy = self.copies().copy_now(workspace)?;
        tilo_workspace_service::workspaces::describe(workspace, &copy.name, &copy.peer)
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
        if self.workspace(workspace).is_none() {
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

    /// Admit a run of the key `key` (certified by `cert`) in `workspace`: the key enters the
    /// workspace as a device of this computer's person, keeping the grant its certificate
    /// carries, and its run acts as the key itself, proving it holds the key.
    pub(crate) async fn admit_key(
        &self,
        workspace: &str,
        key: &SigningKey,
        cert: &tilo_user_identity::DeviceCert,
    ) -> Result<Arc<SessionAdmission>, String> {
        self.copies()
            .enter(workspace, cert)
            .await
            .map_err(|error| error.to_string())?;
        let join = tilo_identity_client::collaboration_join_digest(workspace)
            .map_err(|error| error.to_string())?;
        let presentation = runtime_collab::CertifiedJoinIdentity {
            workspace_id: workspace.into(),
            device_certificate: serde_json::to_vec(cert).map_err(|error| error.to_string())?,
            proof: ed25519_dalek::Signer::sign(key, &join).to_bytes().to_vec(),
        }
        .encode()
        .map_err(|error| error.to_string())?;
        self.copies().admit_key(workspace, cert, presentation).await
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
    /// `fanout`; and tell them each record of theirs whose reach moved, as
    /// this device's own writes reach the workspace's other devices.
    pub(crate) fn follow_admitted_writes(
        &self,
        brokers: Arc<Mutex<tilo_workspace_service::apps::collab_core::BrokerChannels>>,
        fanout: Arc<tilo_workspace_service::apps::collab_broker::FanoutState>,
    ) {
        let delivering = Arc::clone(&fanout);
        tilo_rt::app().spawn(
            tilo_workspace_service::apps::collab_inbound::follow_workspace_writes(
                self.copies(),
                fanout,
                move |document| {
                    tilo_workspace_service::apps::collab_inbound::deliver(
                        &brokers,
                        &delivering,
                        document,
                    );
                },
                || {},
            ),
        );
    }

    /// The record content service of these workspaces, once it is made.
    pub(crate) fn record_content_service(&self) -> Option<Arc<RouteContent>> {
        self.content.get().map(|(content, _)| Arc::clone(content))
    }

    /// Keep `search` as the device's search index.
    pub(crate) fn keep_search(&self, search: Arc<super::search::DeviceSearch>) {
        let _ = self.search.set(search);
    }

    /// Where an app's search entries are published, once the daemon keeps the index.
    pub(crate) fn search_publications(
        &self,
    ) -> Option<Arc<dyn tilo_workspace_service::apps::services::SearchIndexServices>> {
        Some(self.search.get()?.publications())
    }

    /// The search of the device's index, once the daemon keeps one and its broker serves.
    pub(crate) fn search(&self) -> Option<Arc<tilo_workspace_service::search::IndexSearch>> {
        self.search.get()?.search()
    }

    /// The shelf this profile keeps content on, once its record content service is made: where
    /// content put into a workspace here is kept, and read back from.
    pub(crate) fn content_shelf(&self) -> Option<Arc<dyn ContentShelf>> {
        self.content.get().map(|(_, shelf)| Arc::clone(shelf))
    }

    /// Keep `content`, which keeps what it is handed `on` a shelf, as this profile's record
    /// content service from now on.
    pub(crate) fn install_content(&self, content: &Arc<RouteContent>, on: Arc<dyn ContentShelf>) {
        let _ = self.content.set((Arc::clone(content), on));
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

    /// The registries the workspace's replica on this device records: what a moderation
    /// request's destination is checked against here.
    fn package_registries(&self, workspace: &str) -> Option<serde_json::Value> {
        let peer = self.peer(WorkspaceId::parse(workspace)?)?;
        Some(serde_json::json!({ "registries": peer.governance().package_registries() }))
    }

    /// The work kept done in the workspace's replica on this device: what counts a shortcut's
    /// triggered runs here.
    fn done_under(&self, workspace: &str, prefix: &str) -> Vec<serde_json::Value> {
        WorkspaceId::parse(workspace)
            .and_then(|id| self.peer(id))
            .map(|peer| {
                peer.workspace()
                    .done_under(prefix)
                    .into_values()
                    .map(|done| done.answer)
                    .collect()
            })
            .unwrap_or_default()
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
        workspaces
            .copies()
            .commit_own_change(
                &workspace,
                WorkspaceMembershipCommand::Member { change },
                receipt.as_ref(),
            )
            .await
    }
}

impl WorkspaceGrants for MemberListGrants {
    fn may_record(&self, workspace: &str, person: &str) -> bool {
        person == self.person
            && WorkspaceId::parse(workspace)
                .and_then(|id| self.workspaces.workspace(id))
                .is_some_and(|held| {
                    held.policy.may_change_at(
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
            app_action_grant_context(
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

/// What a workspace's history records when a person allows `app_label` to
/// use `target_label`'s action `action_label` in `workspace`, for `lifetime`.
fn app_action_grant_context(
    workspace: &str,
    app_label: &str,
    target_label: &str,
    action_label: &str,
    lifetime: &str,
) -> tilo_actions::ActionContext {
    use tilo_actions::remote::WORKSPACE_GOVERNANCE_PROVIDER;
    tilo_actions::ActionContext::for_resource(
        format!("workspace.permissions:{workspace}"),
        "workspace.permissions",
        "App actions",
    )
    .with_app(WORKSPACE_GOVERNANCE_PROVIDER, "Workspace permissions")
    .with_described(tilo_actions::Described::new(tilo_actions::Did::verb(
        tilo_actions::ActionKey::new(
            WORKSPACE_GOVERNANCE_PROVIDER,
            "workspace.permissions",
            "allow-app-action",
        ),
        [
            ("app".to_owned(), tilo_actions::Scalar::text(app_label)),
            (
                "target".to_owned(),
                tilo_actions::Scalar::text(target_label),
            ),
            (
                "action".to_owned(),
                tilo_actions::Scalar::text(action_label),
            ),
            ("lifetime".to_owned(), tilo_actions::Scalar::text(lifetime)),
        ],
    )))
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

/// The reader groups kept at `path` and those a desktop kept (`carried`, as the file was), as one
/// list: what the key service reads as it takes in the groups.
fn readers_with(path: &Path, carried: &[u8]) -> Result<Vec<u8>, String> {
    let mut readers: Vec<(String, String)> =
        tilo_files::native::atomic::json::load_or_default(path);
    let carried: Vec<(String, String)> = serde_json::from_slice(carried)
        .map_err(|error| format!("the desktop's {READER_GROUPS}: {error}"))?;
    for reader in carried {
        if !readers.contains(&reader) {
            readers.push(reader);
        }
    }
    serde_json::to_vec(&readers).map_err(|error| error.to_string())
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
        collab_types::caps::LaunchGrants {
            sandbox,
            ..collab_types::caps::LaunchGrants::default()
        }
    }

    /// Whether this device's person, the one every launch here is for, is a member of
    /// `workspace`: a device that keeps other people's workspaces too launches into none of
    /// theirs.
    fn serves(&self, workspace: &str) -> bool {
        let person = self.identity.device_cert.user_public_key;
        WorkspaceId::parse(workspace)
            .and_then(|id| self.peer(id))
            .is_some_and(|peer| peer.is_member(&person))
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
                itself: None,
            },
            endpoint.clone(),
            tilo_workspace_service::own_device::key_groups(
                tilo_workspace_service::apps::workspace_keys::KeyProvider::default(),
                None,
                None,
                None,
            ),
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
        let opened = Self::test_of_person().await;
        opened.0.found(workspace, "Home").await.unwrap();
        opened
    }

    /// [`Self::test_hosting_person`] before it founds anything.
    pub(crate) async fn test_of_person() -> (
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
            seal_checks: Arc::new(tilo_workspace_keys::NoSealChecks),
            apps: Some(Arc::clone(&sessions)
                as Arc<dyn tilo_workspace_service::app_serving::AppActionServer>),
            configure: None,
            itself: None,
        })
        .await
        .unwrap();
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
            seal_checks: Arc::new(tilo_workspace_keys::NoSealChecks),
            apps: None,
            configure: None,
            itself: None,
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

/// The device service's member copies, as the login's session reads and writes them.
struct MemberLinks(Arc<WorkspaceService>);

impl tilo_session_client::MemberLinks for MemberLinks {
    fn link(&self, workspace: WorkspaceId) -> Option<tilo_peer_state::direct::DirectSync> {
        self.0.workspaces().member_link(workspace)
    }

    fn take_in(&self, workspace: WorkspaceId, host: tilo_peer_state::join::TransportHint) {
        let copies = self.0.workspaces().clone();
        tokio::spawn(async move { copies.hold_as_members([(workspace, host)]).await });
    }
}
