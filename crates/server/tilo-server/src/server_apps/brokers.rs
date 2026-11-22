//! One app broker for each workspace this server keeps, for as long as it keeps it.
//!
//! A workspace's broker is started when the server starts and finds the workspace, or when a
//! workspace reaches it later, and it is stopped when the workspace goes ([`ServerApps::run`]).
//! Every broker hands the workspace's apps the same three things: what the workspace says of its
//! people and policy ([`ServerWorkspaces`]), the server's commits into it, and the launcher that
//! runs the workspace's packages in this server's processes ([`ServerLauncher`]).

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tilo_desktop_model::{Layout, Workspace, WorkspaceId};
use tilo_workspace_service::apps::collab_broker::{BrokerHandle, FanoutState};
use tilo_workspace_service::apps::collab_core::BrokerChannels;
use tilo_workspace_service::apps::service_reads::ServiceReads;
use tilo_workspace_service::apps::service_workspaces::{ServiceCommits, block_on, describe};
use tilo_workspace_service::apps::services::{
    LauncherServices, ProcessServices, WorkspaceAuthority,
};

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

/// A workspace's broker, running, and what reaches its apps through it.
struct Running {
    _broker: BrokerHandle,
    channels: Arc<Mutex<BrokerChannels>>,
    fanout: Arc<FanoutState>,
}

/// The apps this server runs for the workspaces it keeps.
pub struct ServerApps {
    workspaces: ServerWorkspaces,
    /// Where each workspace's broker listens: one socket per workspace, under this directory.
    sockets: PathBuf,
    /// Where each workspace's invocation journal is kept.
    journals: PathBuf,
    device: [u8; 32],
    processes: ProcessStart,
    running: Mutex<BTreeMap<WorkspaceId, Running>>,
}

impl ServerApps {
    /// The apps of the workspaces `workspaces` keeps, signed by this server's device `device`
    /// and run in `processes`. Each workspace's broker listens under `sockets` and journals under
    /// `journals`.
    #[must_use]
    pub fn new(
        workspaces: ServerWorkspaces,
        sockets: PathBuf,
        journals: PathBuf,
        device: [u8; 32],
        processes: ProcessStart,
    ) -> Arc<Self> {
        Arc::new(Self {
            workspaces,
            sockets,
            journals,
            device,
            processes,
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
            for workspace in kept {
                if apps.running().contains_key(&workspace) {
                    continue;
                }
                match apps.start(workspace) {
                    Ok(running) => {
                        apps.running().insert(workspace, running);
                    }
                    Err(error) => {
                        refused.insert(workspace, error.to_string());
                    }
                }
            }
            refused
        })
        .await;
        match started {
            Ok(refused) => refused,
            Err(error) => std::panic::resume_unwind(error.into_panic()),
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
            socket.clone(),
            hex::encode(self.device),
            self.processes.clone(),
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
                BTreeSet::new(),
                self.workspaces.runtime.clone(),
            ),
            services: ProcessServices {
                committed_documents: Some(Arc::new(ServiceReads::new(
                    self.workspaces.workspaces.clone(),
                ))),
                journal_root: Some(self.journals.join(workspace.to_string())),
                launcher: Some(launcher as Arc<dyn LauncherServices>),
                ..ProcessServices::default()
            },
            channels: Arc::clone(&channels),
            fanout: Arc::clone(&fanout),
        })?;
        Ok(Running {
            _broker: broker,
            channels,
            fanout,
        })
    }
}
