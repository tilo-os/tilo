//! Explicit host services. These are process-owned capabilities, never wire flags.
use qos_desktop::{RoomId, Workspace, WorkspaceTaskStatus, WorkspaceTree};
use std::sync::Arc;

pub use crate::collab_broker_capnp::{
    DocumentBytes, NativeDocumentRead, NativeDocumentReadContext, NativeDocumentReads,
    NativeMediaFuture, NativeMediaRequestContext, NativeMediaServices,
};

/// Reads a fresh immutable projection from the host's committed authority.
/// Mutations go back to that host; this interface owns no replica or persistence.
pub trait WorkspaceAuthority: Send + Sync {
    fn snapshot(&self) -> Result<WorkspaceTree, String>;
    fn legacy_journal_root(&self) -> Option<std::path::PathBuf> {
        None
    }
    /// Records the agent task `task` opened through the desktop's open action,
    /// from the host's own durable record of each open (never from the agent).
    /// Consumed by the task's effective scopes
    /// (`qos_agent_control_protocol::scopes_with_opened_records`).
    fn task_opened_records(
        &self,
        task: &str,
    ) -> Result<Vec<qos_agent_control_protocol::OpenedRecord>, String> {
        let _ = task;
        Ok(Vec::new())
    }
}
/// Legacy native projection only; success is not an aggregate durability receipt.
pub trait LegacyWorkspaceProjection: Send + Sync {
    fn record_effect(
        &self,
        room: &str,
        invocation: tilo_actions::InvocationId,
        actor: &str,
        context: tilo_actions::ActionContext,
        status: tilo_actions::EffectReceiptStatus,
    ) -> Result<(), String>;
    fn record_task_update(
        &self,
        room: &str,
        task: &str,
        actor: &str,
        status: Option<WorkspaceTaskStatus>,
        summary: String,
    ) -> Result<(), String>;
}
pub type WorkspaceAuthorityHandle = Arc<dyn WorkspaceAuthority>;

pub type DesktopCommandFuture = std::pin::Pin<
    Box<
        dyn std::future::Future<
                Output = Result<
                    qos_peer_state::owner_commit::OwnerCommitReceipt,
                    qos_peer_state::desktop_command::DesktopCommandError,
                >,
            > + Send,
    >,
>;
/// An explicitly installed native semantic owner, never a compositor session.
/// The broker constructs requests from authenticated durable invocations.
pub trait DesktopCommands: Send + Sync {
    fn room(&self) -> &str;
    fn execute(
        &self,
        request: qos_peer_state::owner_commit::OwnerCommitRequest,
        direct_delivery: bool,
    ) -> DesktopCommandFuture;
    /// Applications this owner can open (durable app id, display title).
    /// Each is published as an ordinary `desktop:app:<id>#open` action, the
    /// same for people (command palette) and agents.
    fn openable_applications(&self) -> Vec<(String, String)> {
        Vec::new()
    }
    /// [`Self::execute`] for an enrolled agent task. The opener is the broker's
    /// own revalidated task identity; an owner that records opened records
    /// keeps it with the durable result so the task may work in the record it
    /// opened. The default ignores it.
    fn execute_for_agent(
        &self,
        request: qos_peer_state::owner_commit::OwnerCommitRequest,
        opener: DesktopOpener,
    ) -> DesktopCommandFuture {
        let _ = opener;
        self.execute(request, false)
    }
    /// The owner's kept receipt of a committed desktop command (its command
    /// id and the windows it opened), when the owner keeps one. Read after
    /// a successful execute; the default keeps none.
    fn receipt(
        &self,
        request: &qos_peer_state::owner_commit::OwnerCommitRequest,
    ) -> Option<qos_peer_state::desktop_command::DesktopCommandReceipt> {
        let _ = request;
        None
    }
}

/// The enrolled agent task an open-application command runs for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DesktopOpener {
    pub agent: String,
    pub task: String,
}

#[derive(Clone)]
pub struct LaunchableApplication {
    pub package_id: String,
    pub display_name: String,
}
pub trait LauncherServices: Send + Sync {
    fn applications(&self) -> Vec<LaunchableApplication>;
    fn launch(&self, app: &str, workspace: &Workspace) -> Result<(), String>;
    fn attest_process(&self, pid: u32, app: &str, room: &str) -> bool;
}
pub trait ContinuationServices: Send + Sync {
    fn report(
        &self,
        room: RoomId,
        app: String,
        instance: u64,
        schema: u16,
        metadata: Vec<u8>,
    ) -> Result<(), String>;
    fn clear(&self, room: RoomId, app: &str, instance: u64) -> Result<(), String>;
}
/// Called on the broker LocalSet; returned Cap'n Proto clients never cross threads.
pub trait WorkspaceControlServices: Send + Sync {
    fn open(
        &self,
        room: RoomId,
        target: [u8; 32],
        revalidate: Arc<dyn Fn() -> Result<(), capnp::Error> + Send + Sync>,
    ) -> Result<runtime_collab::raw::workspace_control_handle::Client, capnp::Error>;
}

/// The broker-authenticated identity passed to native realtime continuity.
/// The service never accepts caller-supplied routing or peer ownership.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RealtimeRequestContext {
    peer_id: u64,
    app_id: String,
    workspace_id: String,
}

impl RealtimeRequestContext {
    pub(crate) fn authenticated(
        peer_id: u64,
        app_id: impl Into<String>,
        workspace_id: impl Into<String>,
    ) -> Self {
        Self {
            peer_id,
            app_id: app_id.into(),
            workspace_id: workspace_id.into(),
        }
    }

    #[must_use]
    pub const fn peer_id(&self) -> u64 {
        self.peer_id
    }

    #[must_use]
    pub fn app_id(&self) -> &str {
        &self.app_id
    }

    #[must_use]
    pub fn workspace_id(&self) -> &str {
        &self.workspace_id
    }
}

/// Native realtime continuity remains host-owned because it binds portal
/// capture leases and certified device-fabric state. The shared broker owns
/// admission, exact route binding, wire bounds, and disconnect cleanup.
pub trait RealtimeServices: Send + Sync {
    fn command(
        &self,
        context: &RealtimeRequestContext,
        command_json: &[u8],
    ) -> Result<Vec<u8>, String>;

    fn subscribe(
        &self,
        context: &RealtimeRequestContext,
    ) -> Result<tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>, String>;

    fn disconnect(&self, peer_id: u64);
}
#[derive(Clone, Default)]
pub struct HostServices {
    pub certified_agent_reads: Option<Arc<dyn crate::agent_reads::CertifiedAgentReads>>,
    pub committed_documents: Option<Arc<dyn NativeDocumentReads>>,
    pub native_media: Option<Arc<dyn NativeMediaServices>>,
    pub enrolled_agents: Option<Arc<dyn crate::enrolled_dispatch::EnrolledAgentOwner>>,
    /// Durable parent-step path for enrolled task actions with a step identity.
    pub enrolled_steps: Option<Arc<dyn crate::enrolled_dispatch::EnrolledStepService>>,
    pub desktop_commands: Option<Arc<dyn DesktopCommands>>,
    pub native_certificates: Option<Arc<dyn crate::peer_actions::CertificateAuthority>>,
    pub legacy_workspace: Option<Arc<dyn LegacyWorkspaceProjection>>,
    pub launcher: Option<Arc<dyn LauncherServices>>,
    pub continuations: Option<Arc<dyn ContinuationServices>>,
    pub workspace_control: Option<Arc<dyn WorkspaceControlServices>>,
    pub realtime: Option<Arc<dyn RealtimeServices>>,
    pub collaboration_admin: Option<Arc<dyn crate::collaboration_admin::CollaborationAdmin>>,
}
