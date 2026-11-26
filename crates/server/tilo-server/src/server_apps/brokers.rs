//! One app broker for each workspace this server keeps, for as long as it keeps it.
//!
//! A workspace's broker is started when the server starts and finds the workspace, or when a
//! workspace reaches it later, and it is stopped when the workspace goes ([`ServerApps::run`]).
//! Every broker hands the workspace's apps the same three things: what the workspace says of its
//! people and policy ([`ServerWorkspaces`]), the server's commits into it, and the launcher that
//! runs the workspace's packages in this server's processes ([`ServerLauncher`]).
//!
//! The system's apps a server ships are installed in every workspace it serves whose apps it sets
//! up, as the apps they replace were in every workspace: when it starts and finds one that lacks
//! them, and when it starts serving one. A package installed there carries what the native app of
//! its id kept, when its apps start here and when it is installed
//! ([`ServerApps::carry_installed`]); its migrations' marks say whether that was done.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tilo_desktop_model::{Layout, Workspace, WorkspaceId};
use tilo_workspace_service::apps::collab_broker::{BrokerHandle, FanoutState};
use tilo_workspace_service::apps::collab_core::BrokerChannels;
use tilo_workspace_service::apps::package_runs::Sockets;
use tilo_workspace_service::apps::service_reads::ServiceReads;
use tilo_workspace_service::apps::service_workspaces::{ServiceCommits, block_on, describe};
use tilo_workspace_service::apps::services::{
    LauncherServices, ProcessServices, WorkspaceAuthority,
};

use tilo_workspace_service::apps::caller_signer::{self, VerifiedCaller};

use super::{BrokerParts, ProcessStart, ServerLauncher, spawn_broker};
use crate::workspaces::Workspaces;

/// The workspaces this server keeps, as a workspace's broker and the identity service ask about
/// them: each described from what the server keeps of it.
#[derive(Clone)]
pub struct ServerWorkspaces {
    workspaces: Workspaces,
    runtime: tokio::runtime::Handle,
}

impl ServerWorkspaces {
    /// The workspaces `workspaces` keeps, read on `runtime`.
    #[must_use]
    pub const fn new(workspaces: Workspaces, runtime: tokio::runtime::Handle) -> Self {
        Self {
            workspaces,
            runtime,
        }
    }

    /// `workspace` as what this server keeps of it says, or `None` when it keeps none such.
    #[must_use]
    pub fn workspace(&self, workspace: WorkspaceId) -> Option<Workspace> {
        let workspaces = self.workspaces.clone();
        block_on(&self.runtime, async move {
            let (name, peer) = workspaces.governed_copy(workspace).await?;
            describe(workspace, &name, &peer)
        })
    }

    /// The apps `workspace` installed, each the release it runs, by app id.
    #[must_use]
    pub fn installed(&self, workspace: WorkspaceId) -> Vec<tilo_peer_state::InstalledApp> {
        let workspaces = self.workspaces.clone();
        block_on(&self.runtime, async move {
            workspaces
                .governed_copy(workspace)
                .await
                .map(|(_, peer)| peer.governance().app_versions())
                .unwrap_or_default()
        })
    }

    /// What `workspace` allows `caller` now ([`caller_signer::allowed`]).
    ///
    /// # Errors
    /// The server keeps no such workspace, or the caller's device is not on its list.
    pub fn allowed(
        &self,
        workspace: WorkspaceId,
        caller: &VerifiedCaller,
    ) -> Result<tilo_action_policy::MemberAccess, String> {
        caller_signer::allowed(&self.workspaces, &self.runtime, workspace, caller)
    }

    /// Whether `workspace`'s list of devices, as this server holds it, admits `device` now.
    ///
    /// # Errors
    /// The server keeps no such workspace, or its list does not admit the device.
    pub fn admits(&self, workspace: uuid::Uuid, device: [u8; 32]) -> Result<(), String> {
        let id = WorkspaceId::parse(&workspace.to_string()).ok_or("invalid exact workspace")?;
        let workspaces = self.workspaces.clone();
        block_on(&self.runtime, async move {
            let (_, peer) = workspaces
                .governed_copy(id)
                .await
                .ok_or_else(|| format!("this server keeps no workspace {id}"))?;
            peer.permissions(device, tilo_clock::wall())
                .map(drop)
                .map_err(|_| format!("{id} does not admit this device"))
        })
    }

    /// The runtime the server keeps its workspaces on.
    #[must_use]
    pub const fn runtime(&self) -> &tokio::runtime::Handle {
        &self.runtime
    }

    /// What a native app of the id `app` kept in `workspace`, as this server holds it: each of
    /// its documents there by the key a store keeps it under, and none of the records a tilolang
    /// package keeps under the same id (its document and their chunks). While one of them does
    /// not read, none are answered: carrying some of them would leave the rest behind for good.
    ///
    /// # Errors
    /// The workspace is not kept here, or one of the documents does not read.
    pub fn native_documents(
        &self,
        workspace: WorkspaceId,
        app: &str,
    ) -> Result<Vec<(String, automerge::AutoCommit)>, String> {
        let workspaces = self.workspaces.clone();
        let app = app.to_owned();
        block_on(&self.runtime, async move {
            let workspace = workspace.to_string();
            let routes = workspaces
                .workspace_document_routes(&workspace)
                .await
                .map_err(|error| error.to_string())?;
            let mut kept = Vec::new();
            for (of, record) in routes {
                // A tilolang package's record under the same id is its document of the workspace
                // (`""`) and the chunks above it (`derived:chunk.<n>@`).
                if of != app || record.is_empty() || record.starts_with("derived:chunk.") {
                    continue;
                }
                let Some(id) = collab_types::document::DocumentId::parse(&record) else {
                    continue;
                };
                let bytes = workspaces
                    .resource_document(&workspace, &app, &record)
                    .await
                    .map_err(|error| format!("{record}: {error}"))?
                    .ok_or_else(|| format!("{record} has not arrived"))?;
                let document = automerge::AutoCommit::load(&bytes)
                    .map_err(|error| format!("{record}: {error}"))?;
                kept.push((id.storage_key(), document));
            }
            Ok(kept)
        })
    }

    /// The `.tapp` file `workspace` installed as `app`, as the server keeps it.
    #[must_use]
    pub fn installed_package(&self, workspace: &str, app: &str) -> Option<Vec<u8>> {
        let workspaces = self.workspaces.clone();
        let (workspace, app) = (workspace.to_owned(), app.to_owned());
        block_on(&self.runtime, async move {
            workspaces.installed_package(&workspace, &app).await
        })
    }
}

impl tilo_identity::broker::WorkspaceMembership for ServerWorkspaces {
    fn with_workspace(&self, workspace: &WorkspaceId, read: &mut dyn FnMut(&Workspace)) {
        if let Some(described) = self.workspace(*workspace) {
            read(&described);
        }
    }
}

/// One workspace, as its own broker asks about it: the broker of one workspace answers for no
/// other.
struct OneWorkspace {
    workspaces: ServerWorkspaces,
    workspace: WorkspaceId,
}

impl WorkspaceAuthority for OneWorkspace {
    fn with_tree(&self, read: &mut dyn FnMut(&Layout)) -> Result<(), String> {
        let mut tree = Layout::default();
        tree.workspaces
            .extend(self.workspaces.workspace(self.workspace));
        read(&tree);
        Ok(())
    }

    fn with_workspace(
        &self,
        workspace: &str,
        read: &mut dyn FnMut(Option<&Workspace>),
    ) -> Result<(), String> {
        let described = WorkspaceId::parse(workspace)
            .filter(|id| *id == self.workspace && id.to_string() == workspace)
            .and_then(|id| self.workspaces.workspace(id));
        read(described.as_ref());
        Ok(())
    }
}

/// The workspaces' device lists, as their apps' brokers ask whether a device certificate is in
/// force in one: the list that workspace's members, as this server holds them, keeps.
pub(super) struct Certificates {
    workspaces: ServerWorkspaces,
}

impl Certificates {
    /// The device lists of `workspaces`, as a broker asks them.
    pub(super) const fn over(workspaces: ServerWorkspaces) -> Self {
        Self { workspaces }
    }
}
impl tilo_workspace_service::apps::certificate_authority::CertificateAuthority for Certificates {
    fn authorize(
        &self,
        workspace: uuid::Uuid,
        certificate: &tilo_user_identity::DeviceCert,
    ) -> Result<(), String> {
        tilo_user_identity::verify_device_cert(certificate)
            .map_err(|error| format!("the certificate is not in force: {error}"))?;
        self.workspaces
            .admits(workspace, certificate.device_public_key)
    }
}

/// A workspace's broker, running, and what reaches its apps through it.
struct Running {
    _broker: BrokerHandle,
    launcher: Arc<ServerLauncher>,
    /// The releases whose carry ran since the workspace's apps started here: each runs once in
    /// that time, whether its migrations carried anything or found their marks.
    carried: Arc<Mutex<BTreeSet<(String, String)>>>,
    channels: Arc<Mutex<BrokerChannels>>,
    fanout: Arc<FanoutState>,
}

/// The apps this server runs for the workspaces it keeps.
pub struct ServerApps {
    workspaces: ServerWorkspaces,
    /// Where the services of this server's apps listen: one broker socket per workspace, the
    /// identity service's and the content store's.
    sockets: PathBuf,
    /// Where each workspace's invocation journal is kept.
    journals: PathBuf,
    device: [u8; 32],
    processes: ProcessStart,
    shipped: Shipped,
    running: Mutex<BTreeMap<WorkspaceId, Running>>,
}

/// The system's apps a server ships, and the packages its workspaces run, which it keeps.
pub struct Shipped {
    /// The signed packages of the system's apps, each installed in every workspace whose apps
    /// the server sets up.
    pub packages: Vec<Vec<u8>>,
    /// The packages kept here: what tells the ids an app was published under before.
    pub kept: Arc<super::WorkspacePackages>,
}

/// The identity service's socket among a server's app sockets.
pub const IDENTITY_SOCKET: &str = "identity.sock";
/// The content store's socket among a server's app sockets.
pub const CONTENT_SOCKET: &str = "content.sock";

impl ServerApps {
    /// The apps of the workspaces `workspaces` keeps, signed by this server's device `device`
    /// and run in `processes`, with the system's apps it ships installed in each whose apps it
    /// sets up. Their services listen under `sockets`, and each broker journals under `journals`.
    #[must_use]
    pub fn new(
        workspaces: ServerWorkspaces,
        sockets: PathBuf,
        journals: PathBuf,
        device: [u8; 32],
        processes: ProcessStart,
        shipped: Shipped,
    ) -> Arc<Self> {
        Arc::new(Self {
            workspaces,
            sockets,
            journals,
            device,
            processes,
            shipped,
            running: Mutex::new(BTreeMap::new()),
        })
    }

    fn running(&self) -> std::sync::MutexGuard<'_, BTreeMap<WorkspaceId, Running>> {
        self.running
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The workspaces whose brokers run now.
    #[must_use]
    pub fn workspaces(&self) -> BTreeSet<WorkspaceId> {
        self.running().keys().copied().collect()
    }

    /// The workspaces this server keeps, as its apps ask about them.
    #[must_use]
    pub const fn server_workspaces(&self) -> &ServerWorkspaces {
        &self.workspaces
    }

    /// The launcher of `workspace`'s apps, while they run here.
    #[must_use]
    pub fn launcher(&self, workspace: WorkspaceId) -> Option<Arc<ServerLauncher>> {
        self.running()
            .get(&workspace)
            .map(|running| Arc::clone(&running.launcher))
    }

    /// The socket `workspace`'s broker listens on.
    #[must_use]
    pub fn socket(&self, workspace: WorkspaceId) -> PathBuf {
        self.sockets.join(format!("{workspace}.sock"))
    }

    /// Run a broker for each workspace this server keeps, from now until the server stops:
    /// one for each workspace it keeps now, one for each that reaches it later, and none for a
    /// workspace once it goes. Each write another device makes reaches the apps following it.
    pub async fn run(self: Arc<Self>) {
        let mut changes = self.workspaces.workspaces.governance_changes();
        let following = Arc::clone(&self);
        tokio::spawn(
            tilo_workspace_service::apps::collab_inbound::follow_admitted_writes(
                &self.workspaces.workspaces,
                move |document| following.deliver(document),
                || {},
            ),
        );
        self.reconcile_saying().await;
        loop {
            match changes.recv().await {
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    self.reconcile_saying().await;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            }
        }
    }

    /// Hand a write another device made to the apps of its workspace following it.
    fn deliver(&self, document: tilo_workspace_service::apps::collab_inbound::RemoteDocument) {
        let Some(workspace) = WorkspaceId::parse(&document.cluster_id) else {
            return;
        };
        let Some((channels, fanout)) = self
            .running()
            .get(&workspace)
            .map(|running| (Arc::clone(&running.channels), Arc::clone(&running.fanout)))
        else {
            return;
        };
        tilo_workspace_service::apps::collab_inbound::deliver(&channels, &fanout, document);
    }

    /// [`Self::reconcile`], saying why each workspace whose apps did not start did not.
    async fn reconcile_saying(self: &Arc<Self>) {
        for (workspace, error) in self.reconcile().await {
            tracing::warn!(%workspace, %error, "the workspace's apps did not start");
        }
    }

    /// Start the brokers of the workspaces this server keeps that run none, and stop those of
    /// the workspaces it no longer keeps. Answers why each that did not start did not.
    pub async fn reconcile(self: &Arc<Self>) -> BTreeMap<WorkspaceId, String> {
        let kept: BTreeSet<WorkspaceId> = self
            .workspaces
            .workspaces
            .list()
            .await
            .into_iter()
            .filter_map(|summary| WorkspaceId::parse(&summary.workspace))
            .collect();
        let running = self.workspaces();
        for workspace in kept.iter().filter(|workspace| !running.contains(workspace)) {
            self.install_shipped(*workspace).await;
        }
        let apps = Arc::clone(self);
        // Starting a broker binds its socket and starts its thread, and stopping one joins it:
        // never on the runtime's own workers.
        let started = tokio::task::spawn_blocking(move || {
            let gone = {
                let mut running = apps.running();
                let (stay, gone) = std::mem::take(&mut *running)
                    .into_iter()
                    .partition::<BTreeMap<_, _>, _>(|(workspace, _)| kept.contains(workspace));
                *running = stay;
                gone
            };
            drop(gone);
            let mut refused = BTreeMap::new();
            for workspace in &kept {
                if apps.running().contains_key(workspace) {
                    continue;
                }
                match apps.start(*workspace) {
                    Ok(running) => {
                        apps.running().insert(*workspace, running);
                    }
                    Err(error) => {
                        refused.insert(*workspace, error.to_string());
                    }
                }
            }
            for workspace in kept {
                apps.carry_installed(workspace);
            }
            refused
        })
        .await;
        match started {
            Ok(refused) => refused,
            Err(error) => std::panic::resume_unwind(error.into_panic()),
        }
    }

    /// Install each package this server ships that `workspace` does not run yet, when this
    /// server sets up the workspace's apps: as a member installs one, through the workspace's
    /// install action, signed by this server's device for the person it belongs to.
    async fn install_shipped(&self, workspace: WorkspaceId) {
        if self.shipped.packages.is_empty() {
            return;
        }
        let workspaces = &self.workspaces.workspaces;
        let id = workspace.to_string();
        if !workspaces.governs_applications(&id).await.unwrap_or(false) {
            return;
        }
        let cert = workspaces.serving_certificate().clone();
        for package in &self.shipped.packages {
            match workspaces.install_package(&id, &cert, package).await {
                Ok(app) => tracing::info!(%workspace, %app, "the workspace runs the system's app"),
                Err(error) => {
                    tracing::error!(%workspace, %error, "a system app was not installed");
                }
            }
        }
    }

    /// Carry what a native app kept in `workspace` into each package installed there under the
    /// native app's id whose carry has not run since its apps started here: a run that shows
    /// nothing, for the workspace's server, whose migrations carry those documents. The run reads
    /// the server's own copy of that native app's documents in this workspace alone. Its
    /// migrations mark what they carried, so a run that finds their marks carries nothing again
    /// and a carry that failed (a run that could not start, a server stopped while it ran) runs
    /// again on the next start; it ends once its migrations ran, as does a package with none that
    /// read them.
    fn carry_installed(&self, workspace: WorkspaceId) {
        let Some((launcher, carried)) = self
            .running()
            .get(&workspace)
            .map(|running| (Arc::clone(&running.launcher), Arc::clone(&running.carried)))
        else {
            return;
        };
        for installed in self.workspaces.installed(workspace) {
            let app = installed.release.app;
            if !carried
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert((app.clone(), installed.release.package))
            {
                continue;
            }
            let documents = match self.workspaces.native_documents(workspace, &app) {
                Ok(documents) if documents.is_empty() => continue,
                Ok(documents) => documents,
                Err(error) => {
                    tracing::warn!(%workspace, app, %error, "what the native app kept is not read");
                    continue;
                }
            };
            let count = documents.len();
            match launcher.runs().carry(&app, documents) {
                Ok(()) => tracing::info!(
                    %workspace,
                    app,
                    documents = count,
                    "the installed app carries what the native app kept"
                ),
                Err(error) => {
                    tracing::error!(%workspace, app, %error, "what the native app kept is not carried");
                }
            }
        }
    }

    /// The sockets the apps of `workspace` reach this server's services on.
    fn sockets(&self, workspace: WorkspaceId) -> Sockets {
        Sockets {
            collab: self.socket(workspace),
            identity: self.sockets.join(IDENTITY_SOCKET),
            content: self.sockets.join(CONTENT_SOCKET),
        }
    }

    /// Start `workspace`'s broker.
    fn start(&self, workspace: WorkspaceId) -> std::io::Result<Running> {
        use std::os::unix::fs::DirBuilderExt as _;
        // Only this server's own processes reach a workspace's broker, and each workspace's
        // journal is a private directory of its own: both under directories only it may enter.
        for private in [&self.sockets, &self.journals] {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(private)?;
        }
        let socket = self.socket(workspace);
        let launcher = Arc::new(ServerLauncher::new(
            workspace.to_string(),
            self.sockets(workspace),
            hex::encode(self.device),
            self.processes.clone(),
            Arc::clone(&self.shipped.kept),
        ));
        let channels = Arc::new(Mutex::new(BrokerChannels::default()));
        let fanout = Arc::new(FanoutState::new());
        let broker = spawn_broker(BrokerParts {
            socket,
            authority: Arc::new(OneWorkspace {
                workspaces: self.workspaces.clone(),
                workspace,
            }),
            commits: ServiceCommits::new(
                self.workspaces.workspaces.clone(),
                self.device,
                // A new resource's type is the one its installed package declares; until the
                // server reads packages' declarations, it creates none.
                Box::new(|_, _| false),
                self.workspaces.runtime.clone(),
            ),
            services: ProcessServices {
                committed_documents: Some(Arc::new(ServiceReads::new(
                    self.workspaces.workspaces.clone(),
                ))),
                journal_root: Some(self.journals.join(workspace.to_string())),
                launcher: Some(Arc::clone(&launcher) as Arc<dyn LauncherServices>),
                // A certified join is decided against the workspace's own device list: its apps
                // join its collaboration as the devices they run on, and no other.
                native_certificates: Some(Arc::new(Certificates::over(self.workspaces.clone()))),
                ..ProcessServices::default()
            },
            channels: Arc::clone(&channels),
            fanout: Arc::clone(&fanout),
        })?;
        Ok(Running {
            _broker: broker,
            launcher,
            carried: Arc::default(),
            channels,
            fanout,
        })
    }
}
