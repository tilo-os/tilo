//! Every workspace this computer takes part in, as the device's one workspace service holds it.
//!
//! One service holds every workspace: the member copies of the workspaces the login takes part
//! in, synced with the server that hosts them, and, on a computer with no login, the workspace
//! founded here, which the service hosts. The daemon keeps no replica of its own: what it reads
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
use tilo_app_host::services::WorkspaceGrants;
use tilo_desktop_model::{Workspace, WorkspaceId};
use tilo_device_identity::login::Device as Login;
use tilo_identity::broker::WorkspaceMembership;
use tilo_identity::identity_store::DeviceIdentity;
use tilo_invocations::Invocations;
use tilo_peer_state::StatePeer;
use tilo_peer_state::actions::{ReadFuture, Workspaces as PeerWorkspaces};
use tilo_session_client::device::Device as Entered;
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
    /// What its own workspace service kept, as [`tilo_workspace_service::workspaces::MovingService::to_hex`]
    /// hands it over.
    #[serde(default)]
    pub(crate) service: Option<String>,
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
    /// What answers the person's keys' record actions in a workspace hosted here.
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
    /// The certificate the device holds now: the one it started with, until the host that placed
    /// it certified it again ([`Self::renew`]).
    current: std::sync::RwLock<tilo_user_identity::DeviceCert>,
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
    /// The hosts of the workspaces placed on another device, once the person's services run: one
    /// connection per host and workspace, kept for every call that goes there.
    hosts: OnceLock<Arc<super::hosts::Hosts>>,
    /// What changed in the records of these workspaces, as a watch of a whole workspace reads
    /// it: one log a workspace for every connection, made on the first ask.
    changes: OnceLock<Arc<tilo_workspace_service::change_feed::DocumentsFeed>>,
    /// The host of the one workspace this device's certificate confines it to, once its login
    /// named it ([`super::confined`]).
    confined: OnceLock<super::confined::ConfinedHost>,
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
        // A device confined to one workspace issues no certificate, so no service key of its
        // own: its service acts as the device itself. It makes and hosts no workspace, so the
        // two never number changes from one count.
        let itself = itself.or_else(|| {
            super::confined::workspace_of(&identity.device_cert).map(|_| {
                tilo_workspace_service::service::ServiceIdentity {
                    user_public_key: identity.device_cert.user_public_key,
                    device_signing_key: key.clone(),
                    device_cert: identity.device_cert.clone(),
                    revocations: tilo_user_identity::RevocationList::new(),
                }
            })
        });
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
                let config = ServiceConfig {
                    apps,
                    // The language runtime the program mounted: the service's own roles read and
                    // change the built-in apps' records through it, and offer the styles the
                    // device's installed packages declare.
                    tilolang: super::capabilities::tilolang(),
                    // What the device keeps for its people: their accounts, models, vault and
                    // what they call important.
                    registers: vec![tilo_person_services::register],
                    ..config
                };
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
            current: std::sync::RwLock::new(identity.device_cert.clone()),
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
            hosts: OnceLock::new(),
            changes: OnceLock::new(),
            confined: OnceLock::new(),
        }
    }

    /// The one workspace this device's certificate confines it to (a workspace's VM), when it
    /// does: it keeps no copy of any workspace then, and reaches that one through its host.
    pub(crate) fn confined(&self) -> Option<WorkspaceId> {
        super::confined::workspace_of(&self.identity.device_cert)
    }

    /// Reach the workspace this device is confined to through `host` from now on: the host its
    /// login entered through, the one device whose requests it answers, and the one it takes its
    /// certificate again from ([`super::confined::Renewals`]).
    pub(crate) fn confine_to(self: &Arc<Self>, host: super::confined::ConfinedHost) {
        let device = host.device;
        if self.confined.set(host).is_ok()
            && let Some(workspace) = self.confined()
        {
            let copies = self.copies();
            copies
                .action_service()
                .answer_only(device, workspace.to_string());
            copies.providers().answer_placing_host(device);
            copies.provide(
                tilo_action_services::environment_window::PLACEMENT,
                Arc::new(super::confined::Renewals(Arc::downgrade(self))),
            );
        }
    }

    /// Hold `cert`, the device certified again, from now on: what [`Self::device_cert`] answers
    /// and what the device proves itself with on its next link. The caller checked it is this
    /// device's ([`super::confined::take_renewal`]).
    pub(crate) fn renew(&self, cert: tilo_user_identity::DeviceCert) {
        self.copies().action_service().answer_under(cert.clone());
        *self
            .current
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = cert;
    }

    /// The host of the workspace this device is confined to, once its login named it.
    pub(crate) fn confined_host(&self) -> Option<&super::confined::ConfinedHost> {
        self.confined.get()
    }

    /// Keep the device's records `records`, once the person's services run: what the settings
    /// actions write and what reads in the person's workspace answer.
    pub(crate) fn hold_device_records(&self, records: Arc<super::records::DeviceRecords>) {
        let _ = self.records.set(records);
    }

    /// Keep the hosts of the workspaces placed on another device, once the person's services
    /// run: the connections a person's own calls of what a host keeps go on.
    pub(crate) fn hold_hosts(&self, hosts: Arc<super::hosts::Hosts>) {
        let _ = self.hosts.set(hosts);
    }

    /// The hosts of the workspaces placed on another device, once the person's services run.
    pub(crate) fn hosts(&self) -> Option<Arc<super::hosts::Hosts>> {
        self.hosts.get().cloned()
    }

    /// The records this device keeps as its own, once the person's services kept them.
    pub(crate) fn device_records(&self) -> Option<&Arc<super::records::DeviceRecords>> {
        self.records.get()
    }

    /// Found `workspace`, called `name`, here unless the service holds it already, and enter it
    /// as this device: the workspace of a computer with no login, whose member list names the
    /// person as its owner. Answers whether it was this call that founded it: a workspace the
    /// service holds is entered, not made again.
    ///
    /// # Errors
    /// The service refused to found it, or to let this device in.
    pub(crate) async fn found(&self, workspace: WorkspaceId, name: &str) -> Result<bool, String> {
        let id = workspace.to_string();
        let cert = &self.identity.device_cert;
        let made = self.service.workspaces().summary(&id).await.is_err();
        self.service.hold_own_workspace(&id, name, cert).await?;
        self.service
            .workspaces()
            .enter(&id, (cert, &*self.identity.signer))
            .await
            .map(drop)
            .map_err(|error| error.to_string())?;
        Ok(made)
    }

    /// [`Self::found`], and when this call made the workspace, the entry `entry` names: the
    /// founder's first change, settled through the same entry change an owner makes later
    /// (the workspace service's `found_entry`), and never again over what the workspace's
    /// owners chose since.
    ///
    /// # Errors
    ///
    /// [`Self::found`]'s, or why the entry cannot stand.
    pub(crate) async fn found_kept(
        &self,
        workspace: WorkspaceId,
        name: &str,
        entry: Option<&tilo_action_policy::EntryPolicy>,
    ) -> Result<bool, String> {
        let made = self.found(workspace, name).await?;
        if made && let Some(entry) = entry {
            self.service
                .workspaces()
                .found_entry(
                    &workspace.to_string(),
                    &self.identity.device_cert,
                    entry.clone(),
                )
                .await
                .map_err(|error| error.to_string())?;
        }
        Ok(made)
    }

    /// Take in, once, what a desktop of this device kept itself before this service kept its
    /// person's workspaces (`account/workspaces#import`): each key group the desktop sealed, opened
    /// with the key it was sealed with and sealed again under this device's vault beside the groups
    /// the key service keeps, which opens it once the service takes part in its workspace; and each
    /// workspace the desktop took part in, entered here as a member at the address its host
    /// signed. A group the key service keeps already stays as it is: the device keeps the one leaf
    /// it has in each group, and no group is founded again beside one the desktop had. The
    /// workspaces the desktop's own service hosted move here, after the groups
    /// ([`tilo_workspace_service::workspaces::Workspaces::move_in`]), and are answered beside
    /// the answer. The person's own workspace is never founded here: it was founded where their
    /// root was made, and comes in with the desktop's service or from a device that has it.
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
        let answer = serde_json::json!({
            "groups": groups.iter().map(ToString::to_string).collect::<Vec<_>>(),
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
    pub(crate) fn keys(&self) -> tilo_app_host::workspace_keys::KeyProvider {
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

    /// Keep `device`, the login's session: each workspace it entered was taken in here as its host
    /// let it in ([`MemberLinks`]), and this service is the device's one copy of them.
    ///
    /// A device that takes its server's work enters itself among its person's servers through
    /// its first session, at the address its endpoint answers on, as a server does on each start:
    /// the server finds it there for its workspaces' Linux turns.
    pub(crate) async fn hold(&self, device: &Entered) {
        // A device confined to one workspace keeps no copy of it, nor of the person's own: its
        // host keeps them, and answers what it asks there.
        if self.confined().is_some() {
            let _ = self.session.set(device.clone());
            return;
        }
        if self.session.set(device.clone()).is_ok()
            && self.takes_work.load(std::sync::atomic::Ordering::Relaxed)
        {
            let me = (Some(device), &self.identity.device_cert);
            let at = Some(self.endpoint.addr());
            if let Err(error) = super::ticket::server_entry(self.copies(), me, at).await {
                tracing::warn!(%error, "this device was not entered among its person's servers");
            }
        }
    }

    /// Tell the device this server's `login` entered where it answers: that device took it in by
    /// its key alone, and hands it the person's workspaces there.
    pub(crate) async fn enter_among_servers(&self, login: &Login) -> Result<(), String> {
        let session =
            tilo_api::daemon::enter::connect_signed(self.signer(), login, Some(self.lent()))
                .await
                .map_err(|error| format!("{error:#}"))?;
        let me = (Some(&session), &self.identity.device_cert);
        let told = super::ticket::server_entry(self.copies(), me, Some(self.endpoint.addr())).await;
        session.close();
        told
    }

    /// Close the service and its endpoint, as the process ending would: what the service holds
    /// is kept, and the next service opened under this device's directory takes it up.
    #[cfg(test)]
    pub(crate) async fn shutdown(&self) {
        self.shutdown_service().await;
        self.close_endpoint().await;
    }

    /// Close the service alone, as the person's part that ends does: what it holds is kept, and
    /// the next part opens the same stores. The endpoint it ran on is handed to that part
    /// rather than closed with it, so the port this device is reached at is never bound again.
    pub(crate) async fn shutdown_service(&self) {
        self.service.shutdown().await;
    }

    /// Close the device's endpoint, awaited: the socket it bound is the one the part that starts
    /// next binds, so it goes before what the part still has to let go of behind it — a folder
    /// whose mount will not unmount, a service still saving — rather than with it.
    pub(crate) async fn close_endpoint(&self) {
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

    /// Hear every device's hello this build's peer protocol does not work with, by `hook`: what
    /// tells the person which side updates ([`super::version_notice`]).
    pub(crate) fn observing_versions(
        &self,
        hook: std::sync::Arc<tilo_peer_state::direct::Versions>,
    ) {
        self.copies().observing_versions(hook);
    }

    /// What the service's workspaces keep for their people.
    pub(crate) fn people(&self) -> tilo_person_services::PersonServices {
        tilo_person_services::PersonServices::of(self.copies())
    }

    /// Share the device's record of the invocations it was asked for with the workspace
    /// service, so what a model call costs is written into the record of the invocation that
    /// made it ([`Invocations::note_spend`]).
    pub(crate) fn serve_invocations(&self, records: Arc<Invocations>) {
        self.people().serve_invocations(records);
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

    /// The device certificate this profile's ownership holds now.
    pub(crate) fn device_cert(&self) -> tilo_user_identity::DeviceCert {
        self.current
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// The person this device belongs to: whose devices may ask it for work, and whose own ask
    /// stops an agent session here.
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

    /// Whether this device keeps `workspace` now: its member copy, or the workspace it hosts
    /// here. Read synchronously, as [`Self::holds`] is.
    pub(crate) fn keeps_now(&self, workspace: WorkspaceId) -> bool {
        self.copies().copy_now(workspace).is_some()
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
                .and_then(|copy| copy.peer.governance().member_policy().ok())
        }
    }

    /// `workspace`'s replica as this device holds it: its member copy, or the
    /// workspace the service hosts here.
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
        self.copies().workspaces_now()
    }

    /// One workspace as [`Self::workspaces`] describes it, built alone.
    pub(crate) fn workspace(&self, workspace: WorkspaceId) -> Option<Workspace> {
        self.copies().workspace_now(workspace)
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
            self.signer().device_public_key(),
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
    /// This device does not have the workspace, or the actor
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
            .enter(workspace, (cert, key))
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
        brokers: Arc<Mutex<tilo_app_host::collab_core::BrokerChannels>>,
        fanout: Arc<tilo_app_host::collab_broker::FanoutState>,
    ) {
        let delivering = Arc::clone(&fanout);
        tilo_rt::app().spawn(tilo_app_host::collab_inbound::follow_workspace_writes(
            self.copies(),
            fanout,
            move |document| {
                tilo_app_host::collab_inbound::deliver(&brokers, &delivering, document);
            },
            || {},
        ));
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
    pub(crate) fn search_index_services(
        &self,
    ) -> Option<Arc<dyn tilo_app_host::services::SearchIndexServices>> {
        Some(self.search.get()?.publications())
    }

    /// What changed in the records of these workspaces: the feed a watch of a whole workspace
    /// reads, and the documents the service holds that it reads them from.
    pub(crate) fn change_feed(
        &self,
    ) -> (
        Arc<tilo_workspace_service::change_feed::DocumentsFeed>,
        tilo_workspace_service::device_documents::ServiceDocuments,
    ) {
        let feed = Arc::clone(self.changes.get_or_init(|| {
            Arc::new(tilo_workspace_service::change_feed::DocumentsFeed::new(
                Arc::new(ui::history_source::ThreadWords),
            ))
        }));
        (
            feed,
            tilo_workspace_service::device_documents::ServiceDocuments::of(self.copies().clone()),
        )
    }

    /// The search of the device's index, once the daemon keeps one and its broker serves.
    pub(crate) fn search(&self) -> Option<Arc<tilo_workspace_search::IndexSearch>> {
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

impl WorkspaceMembership for ServiceWorkspaces {
    fn with_workspace(&self, workspace: &WorkspaceId, read: &mut dyn FnMut(&Workspace)) {
        if let Some(held) = self.workspace(*workspace) {
            read(&held);
        }
    }
}

/// The grants of a workspace this device takes back: each a change to the workspace's member
/// list, committed through the service as every other member's change is.
pub(crate) struct MemberListGrants {
    pub(crate) workspaces: Arc<ServiceWorkspaces>,
}

impl WorkspaceGrants for MemberListGrants {
    // The member copy's list, sent to the device serving it, or the list of the workspace
    // hosted here.
    fn spend(&self, workspace: &str, grant: &str) -> bool {
        self.workspaces
            .copies()
            .spend_once_now(workspace, &tilo_action_policy::GrantId::new(grant))
    }
}

/// The agents each workspace this device keeps names in its own record of its agents: whom the
/// broker admits as an agent acting for its person there, read again each time it asks, whether
/// or not a desktop session runs. What each spent is counted by the desktop session.
pub(crate) struct RecordedAgents {
    workspaces: std::sync::Weak<ServiceWorkspaces>,
    session: Arc<tilo_app_host::seat::SeatAgents>,
    /// Each workspace's agents as its list was last read, followed from its writes: what the
    /// app runtime's own thread is answered from, since it never waits on the workspaces.
    followed: Arc<tilo_workspace_service::workspaces::FollowedAgents>,
}

impl RecordedAgents {
    /// The agents `workspaces` record, followed from now on the app runtime.
    pub(crate) fn new(
        workspaces: &Arc<ServiceWorkspaces>,
        session: Arc<tilo_app_host::seat::SeatAgents>,
    ) -> Self {
        let followed = tilo_workspace_service::workspaces::FollowedAgents::new(workspaces.copies());
        tilo_rt::app().spawn(Arc::clone(&followed).follow());
        Self {
            workspaces: Arc::downgrade(workspaces),
            session,
            followed,
        }
    }
}

impl tilo_app_host::services::AgentRoster for RecordedAgents {
    fn listed(&self, workspace: &str) -> Vec<tilo_app_host::services::ListedAgent> {
        let Some(workspaces) = self.workspaces.upgrade() else {
            return Vec::new();
        };
        // The app runtime's own thread never waits on the workspaces: it is answered from the
        // list as last followed, which names nobody while a write of it is newer than the read.
        // Any other thread reads the list as it stands.
        if tilo_rt::app().is_runtime_thread() {
            return self.followed.listed(workspace);
        }
        let workspace = workspace.to_owned();
        block_on_owned(async move { workspaces.copies().listed_agents(&workspace).await })
    }

    fn spend(&self, workspace: &str, agent: &str) -> Option<runtime_collab::AgentSpend> {
        self.session.spend(workspace, agent)
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
        None => workspaces.hosted_copy(workspace).await,
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
    /// governance records, whether this device hosts it or holds it as a
    /// member copy.
    fn launch_grants(&self, workspace: &str) -> tilo_permissions::caps::LaunchGrants {
        let Some(id) = WorkspaceId::parse(workspace) else {
            return tilo_permissions::caps::LaunchGrants::default();
        };
        let Some(sandbox) = self
            .peer(id)
            .and_then(|peer| peer.governance().member_sandbox().ok())
        else {
            return tilo_permissions::caps::LaunchGrants::default();
        };
        tilo_permissions::caps::LaunchGrants {
            sandbox,
            ..tilo_permissions::caps::LaunchGrants::default()
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

    /// What `app` may do in `workspace` for this device's person: the workspace's policy, as
    /// this device holds it now, narrowed to the app as a session's push narrows it.
    fn app_access(&self, workspace: &str, app: &str) -> Option<tilo_action_policy::MemberAccess> {
        let id = WorkspaceId::parse(workspace)?;
        let policy = self.policy_of(id)()?;
        let person = hex::encode(self.device_cert().user_public_key);
        Some(
            collab_types::action_authority::AppActionAuthorityProjection::access_for(
                &policy, app, &person,
            ),
        )
    }

    /// Whether `workspace`, as this device holds its policy now, lets `app` keep its own records
    /// by itself: the same decision every admission of the app's own writes makes.
    fn app_keeps_its_records(&self, workspace: &str, app: &str) -> bool {
        WorkspaceId::parse(workspace)
            .and_then(|id| self.policy_of(id)())
            .is_some_and(|policy| {
                tilo_action_policy::roles::app_keeps_its_records(
                    &policy,
                    app,
                    &tilo_action_policy::Context::at_time(tilo_clock::wall()),
                )
            })
    }
}

/// The workspace replicas the runtime's own services read: the profile's own
/// workspace through the service that hosts it, and each member copy.
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
    /// A service hosting `workspace` alone, on a profile of its own, with a
    /// synthetic device its host signed: what a test of the runtime's own
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
                tilo_app_host::workspace_keys::KeyProvider::default(),
                None,
                None,
                None,
            ),
            // The fixture's endpoint names no address anywhere: nothing will,
            // so opening does not wait for one.
            |config| tilo_workspace_service::service::ServiceConfig {
                reaching: tilo_workspace_service::Reaching::OnItsOwn,
                tilolang: super::capabilities::tilolang(),
                registers: vec![tilo_person_services::register],
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

    /// A service hosting `workspace` alone, on a profile of its own, as a machine whose person
    /// this account holds: a real identity store, its own key service, and no login. What the
    /// stack's own person is, and what a test that founds the person's Servers workspace needs.
    /// Answers the device's key and certificate, and the key taken as the device's daemon takes
    /// it when it starts: the service is that daemon's, and signs as the device while it lives.
    pub(crate) async fn test_hosting_person(
        workspace: WorkspaceId,
    ) -> (
        Arc<Self>,
        tempfile::TempDir,
        ed25519_dalek::SigningKey,
        tilo_user_identity::DeviceCert,
        tilo_device_identity::persist::DeviceKeyLock,
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
        tilo_device_identity::persist::DeviceKeyLock,
    ) {
        let directory = tempfile::tempdir().unwrap();
        let paths = tilo_device_identity::IdentityPaths::under(directory.path().join("keys"));
        let taken = tilo_identity::provisioning::device_key_taken_as_daemon(&paths).unwrap();
        tilo_identity::provisioning::create_person(
            directory.path(),
            &paths,
            "",
            "Synthetic runtime",
            tilo_identity::provisioning::IdentityStorage::Files,
        )
        .unwrap();
        Self::test_of_device(directory, paths, taken).await
    }

    /// [`Self::test_of_person`] for the person the account `uid` founded on this computer: their
    /// root kept for that account alone, where a call arriving as it signs with it, and this
    /// computer's one device key certified by it.
    pub(crate) async fn test_of_account_person(
        uid: u32,
    ) -> (
        Arc<Self>,
        tempfile::TempDir,
        ed25519_dalek::SigningKey,
        tilo_user_identity::DeviceCert,
        tilo_device_identity::persist::DeviceKeyLock,
    ) {
        let paths = tilo_api::calls::device_keys();
        let taken = tilo_identity::provisioning::device_key_taken_as_daemon(&paths).unwrap();
        super::services::found_account_person(&paths, "Synthetic runtime", uid).unwrap();
        Self::test_of_device(tempfile::tempdir().unwrap(), paths, taken).await
    }

    /// The service kept below `directory` as the device whose key and certificate are at `paths`,
    /// its key `taken` as its daemon.
    async fn test_of_device(
        directory: tempfile::TempDir,
        paths: tilo_device_identity::IdentityPaths,
        taken: tilo_device_identity::persist::DeviceKeyLock,
    ) -> (
        Arc<Self>,
        tempfile::TempDir,
        ed25519_dalek::SigningKey,
        tilo_user_identity::DeviceCert,
        tilo_device_identity::persist::DeviceKeyLock,
    ) {
        let key = taken.key().clone();
        let cert = tilo_device_identity::persist::read_owner_cert(&paths)
            .unwrap()
            .unwrap();
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
        (workspaces, directory, key, cert, taken)
    }

    /// The service a logged-in computer keeps below `dir` as the device `key` certified by
    /// `cert`, on an endpoint bound the way its session reaches `server`.
    pub(crate) async fn open_for_test(
        dir: &Path,
        key: &SigningKey,
        cert: &tilo_user_identity::DeviceCert,
        server: &tilo_server_client::ServerAddress,
    ) -> Result<Arc<Self>, String> {
        Self::open_for_test_as(dir, key, cert, server, None).await
    }

    /// [`Self::open_for_test`] for a machine that serves workspaces for its person, as a server
    /// does: it acts as its device, under the device's own key and certificate, so a workspace
    /// handed to it answers with the device the hand-over reached.
    pub(crate) async fn open_for_test_as_server(
        dir: &Path,
        key: &SigningKey,
        cert: &tilo_user_identity::DeviceCert,
        server: &tilo_server_client::ServerAddress,
    ) -> Result<Arc<Self>, String> {
        let itself = Some(tilo_workspace_service::service::ServiceIdentity::as_device(
            cert,
            key.clone(),
        ));
        Self::open_for_test_as(dir, key, cert, server, itself).await
    }

    /// [`Self::open_for_test`], with `itself` the identity its service acts as: the device's own,
    /// carrying the revocations a server holds.
    async fn open_for_test_as(
        dir: &Path,
        key: &SigningKey,
        cert: &tilo_user_identity::DeviceCert,
        server: &tilo_server_client::ServerAddress,
        itself: Option<tilo_workspace_service::service::ServiceIdentity>,
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
            itself,
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
/// the future is driven from a thread of its own there. The app runtime's own
/// thread never waits here: every task of every session and daemon in the
/// process runs on it, and the future waits on that thread's timers and
/// sockets, so the wait would hold them all until something else gave way.
/// A debug build stops there, and a release build says where it waits from: the call site,
/// which a build without symbols still names, and the backtrace.
#[track_caller]
pub(crate) fn block_on_owned<F>(future: F) -> F::Output
where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
{
    let runtime = tilo_rt::app();
    if runtime.is_runtime_thread() {
        let at = std::panic::Location::caller();
        tracing::error!(
            %at,
            from = %std::backtrace::Backtrace::force_capture(),
            "a synchronous call into the workspaces waits on the app runtime's own thread"
        );
        debug_assert!(
            false,
            "a synchronous call into the workspaces waits on the app runtime's own thread, at {at}"
        );
    }
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

#[cfg(test)]
#[path = "entry_join_tests.rs"]
mod entry_join_tests;

/// The device service's member copies, as the login's session reads and writes them.
struct MemberLinks(Arc<WorkspaceService>);

impl tilo_session_client::MemberLinks for MemberLinks {
    fn link(&self, workspace: WorkspaceId) -> Option<tilo_peer_state::direct::DirectSync> {
        self.0.workspaces().member_link(workspace)
    }

    fn take_in(&self, workspace: WorkspaceId, host: tilo_peer_state::join::TransportHint) {
        let copies = self.0.workspaces().clone();
        tokio::spawn(async move {
            // A workspace the person is let into is theirs on every device: listed, and taken in
            // by each device's follower of the list, this one's too. Their own workspace, which
            // holds the list, a device that holds no copy of it yet holds itself.
            if copies.takes_in_by_list(&workspace.to_string()).await {
                if let Err(why) = Box::pin(copies.take_in_joined(workspace, &host)).await {
                    tracing::warn!(%workspace, %why, "a workspace this device was let into is not here yet");
                }
            } else {
                Box::pin(copies.hold_as_members([(workspace, host)])).await;
            }
        });
    }

    fn up(&self, workspace: WorkspaceId) -> Option<tokio::sync::watch::Receiver<bool>> {
        self.0.workspaces().member_up(workspace).ok()
    }

    fn own_admissions(&self, workspace: WorkspaceId, device: &[u8; 32]) -> Vec<Vec<u8>> {
        self.0.workspaces().own_admissions(workspace, device)
    }

    fn keep_own_admission(
        &self,
        workspace: WorkspaceId,
        device: &[u8; 32],
        changes: &[Vec<u8>],
    ) -> Result<(), String> {
        self.0
            .workspaces()
            .keep_own_admission(workspace, device, changes)
    }

    fn forget_own_admission(&self, workspace: WorkspaceId, device: &[u8; 32]) {
        self.0.workspaces().forget_own_admission(workspace, device);
    }
}
