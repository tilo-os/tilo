//! Explicit host services. These are process-owned capabilities, never wire flags.
use qos_desktop::{RoomId, Workspace, WorkspaceTaskStatus, WorkspaceTree};
use std::sync::Arc;

/// Reads a fresh immutable projection from the host's committed authority.
/// Mutations go back to that host; this interface owns no replica or persistence.
pub trait WorkspaceAuthority: Send + Sync {
    fn snapshot(&self) -> Result<WorkspaceTree, String>;
    fn legacy_journal_root(&self) -> Option<std::path::PathBuf> {
        None
    }
}
/// Legacy native projection only; success is not an aggregate durability receipt.
pub trait LegacyWorkspaceProjection: Send + Sync {
    fn record_effect(
        &self,
        room: &str,
        invocation: qos_actions::InvocationId,
        actor: &str,
        context: qos_actions::ActionContext,
        status: qos_actions::EffectReceiptStatus,
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

pub type DesktopCommandFuture = std::pin::Pin<Box<dyn std::future::Future<Output = Result<qos_peer_state::owner_commit::OwnerCommitReceipt, qos_peer_state::desktop_command::DesktopCommandError>> + Send>>;
/// An explicitly installed native semantic owner, never a compositor session.
/// The broker constructs requests from authenticated durable invocations.
pub trait DesktopCommands: Send + Sync {
    fn room(&self) -> &str;
    fn execute(&self, request: qos_peer_state::owner_commit::OwnerCommitRequest) -> DesktopCommandFuture;
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
#[derive(Clone, Default)]
pub struct HostServices {
    pub desktop_commands: Option<Arc<dyn DesktopCommands>>,
    pub native_certificates: Option<Arc<dyn crate::peer_actions::CertificateAuthority>>,
    pub legacy_workspace: Option<Arc<dyn LegacyWorkspaceProjection>>,
    pub launcher: Option<Arc<dyn LauncherServices>>,
    pub continuations: Option<Arc<dyn ContinuationServices>>,
    pub workspace_control: Option<Arc<dyn WorkspaceControlServices>>,
}
